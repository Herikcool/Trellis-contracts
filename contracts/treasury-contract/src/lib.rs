#![no_std]
//! # Treasury Contract
//!
//! Protocol treasury management for Trellis humanitarian aid platform.
//!
//! ## Overview
//!
//! The Treasury Contract is the financial heart of Trellis, managing:
//! - **Multi-token per-category balances** (e.g., `reserve`, `rewards`) for fund segregation
//! - **Withdrawal limits** to prevent accidental large transfers
//! - **Role-based access control** via treasury managers and administrators
//! - **Referral reward distribution** through integration with the referral contract
//!
//! ## Categories
//!
//! The treasury organizes funds into categories per asset, each with its own balance:
//! - **`reserve`**: Protocol emergency funds
//! - **`rewards`**: Referral commission pool
//! - Custom categories as defined by administrators
//!
//! ## Roles
//!
//! - **Admin**: Full governance (set managers, configure limits, emergency withdraw)
//! - **Treasury Manager**: Operational access (deposit, withdraw, distribute rewards)
//!
//! ## Queries
//!
//! - [`TreasuryContract::category_balance`]: Check an asset category's current balance
//! - [`TreasuryContract::withdrawal_limit`]: View the max per-transaction limit
//! - [`TreasuryContract::referral_contract`]: See the registered referral contract

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Bytes, BytesN, Env, Symbol};

use shared::auth::{self, Permission, Role};
use shared::errors::Error;
use shared::events::{
    self, emit_action_executed, emit_commission_paid, emit_module_initialized,
    emit_permission_changed, emit_treasury_deposit, emit_treasury_withdrawal,
};
use shared::storage::{instance_get, instance_set, persistent_set};
use shared::{record_action_audit_event, ResourceLink, TimelineEventType};

/// Storage key prefix for per-category multi-token balances; the full key is
/// `(BALANCE, token, category)`.
const BALANCE: Symbol = symbol_short!("cat_bal");
/// Storage key for the configurable max per-transaction withdrawal limit.
const MAX_WD: Symbol = symbol_short!("max_wd");
/// Category symbol for the emergency reserve (used by `emergency_withdraw`).
const RESERVE_CATEGORY: Symbol = symbol_short!("reserve");
/// Category symbol for the referral commission rewards pool (used by
/// `distribute_reward`).
const REWARDS_CATEGORY: Symbol = symbol_short!("rewards");
/// Storage key for the referral contract address authorised to call
/// `distribute_reward`.
const REFERRAL_CONTRACT: Symbol = symbol_short!("ref_ctr");
/// Storage key prefix for scheduled-action time windows; full key is
/// `(SCHEDULE, action_id)`.
const SCHEDULE: Symbol = symbol_short!("sched");

/// A time window during which a scheduled action may execute.
///
/// `not_before` is the earliest ledger timestamp at which the action is valid;
/// `expires_at` is the exclusive upper bound (the action is stale at or after
/// this timestamp). Both are unix seconds sourced from `env.ledger().timestamp()`.
#[soroban_sdk::contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeWindow {
    pub not_before: u64,
    pub expires_at: u64,
}

impl TimeWindow {
    /// Validates `now` against this window, returning a typed error for the
    /// early / late / stale cases so callers can surface precise diagnostics.
    pub fn validate(&self, now: u64) -> Result<(), Error> {
        if self.expires_at <= self.not_before {
            return Err(Error::InvalidArgument);
        }
        if now < self.not_before {
            return Err(Error::ActionTooEarly);
        }
        if now >= self.expires_at {
            return Err(Error::ActionExpired);
        }
        Ok(())
    }
}

/// Validates `now` against a window and emits an audit event on failure so
/// rejected attempts are observable on-chain.
fn validate_window(
    env: &Env,
    actor: &Address,
    action: Symbol,
    window: &TimeWindow,
) -> Result<(), Error> {
    let now = env.ledger().timestamp();
    match window.validate(now) {
        Ok(()) => Ok(()),
        Err(err) => {
            let reason = match err {
                Error::ActionTooEarly => symbol_short!("early"),
                Error::ActionExpired => {
                    if now >= window.expires_at {
                        symbol_short!("stale")
                    } else {
                        symbol_short!("late")
                    }
                }
                _ => symbol_short!("invalid"),
            };
            record_treasury_audit(
                env,
                actor,
                TimelineEventType::ActionRejected,
                action,
                reason,
                None,
                None,
                Some(window.not_before as i128),
                Some(window.expires_at as i128),
            )?;
            Err(err)
        }
    }
}

fn record_treasury_audit(
    env: &Env,
    actor: &Address,
    event_type: TimelineEventType,
    action: Symbol,
    reason: Symbol,
    resource: Option<Address>,
    attribute: Option<Symbol>,
    before: Option<i128>,
    after: Option<i128>,
) -> Result<(), Error> {
    record_action_audit_event(
        env,
        actor,
        event_type,
        ResourceLink {
            kind: Bytes::from_slice(env, b"treasury"),
            id: 0,
            revision: 0,
        },
        symbol_short!("treasury"),
        action,
        reason,
        resource,
        attribute,
        before,
        after,
    )?;
    Ok(())
}

fn zero_correlation_id(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0; 32])
}

#[contract]
pub struct TreasuryContract;

#[contractimpl]
impl TreasuryContract {
    /// Initialise the contract: sets the admin address, grants the admin
    /// the `TreasuryManager` role, and sets the initial max
    /// per-transaction withdrawal limit.
    pub fn initialize(env: Env, admin: Address, max_withdrawal_limit: i128) -> Result<(), Error> {
        if max_withdrawal_limit <= 0 {
            return Err(Error::InvalidArgument);
        }
        auth::initialize_admin(&env, &admin)?;
        persistent_set(
            &env,
            &shared::auth::DataKey::Role(admin.clone(), Role::TreasuryManager),
            &true,
        );
        instance_set(&env, &MAX_WD, &max_withdrawal_limit);
        record_treasury_audit(
            &env,
            &admin,
            TimelineEventType::ConfigChanged,
            symbol_short!("init"),
            symbol_short!("setup"),
            None,
            None,
            None,
            Some(max_withdrawal_limit),
        )?;
        emit_module_initialized(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            1,
            &admin,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Grants the `TreasuryManager` role to `who`. Admin only.
    pub fn add_treasury_manager(env: Env, caller: Address, who: Address) -> Result<(), Error> {
        let was_manager = auth::has_role(&env, &who, Role::TreasuryManager);
        auth::grant_role(&env, &caller, &who, Role::TreasuryManager)?;
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::RoleChanged,
            symbol_short!("mgr_grnt"),
            symbol_short!("adm_grant"),
            Some(who.clone()),
            Some(symbol_short!("manager")),
            Some(if was_manager { 1 } else { 0 }),
            Some(1),
        )?;
        emit_permission_changed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("manager"),
            &who,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Revokes the `TreasuryManager` role from `who`. Admin only.
    pub fn remove_treasury_manager(env: Env, caller: Address, who: Address) -> Result<(), Error> {
        let was_manager = auth::has_role(&env, &who, Role::TreasuryManager);
        auth::revoke_role(&env, &caller, &who, Role::TreasuryManager)?;
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::RoleChanged,
            symbol_short!("mgr_rvok"),
            symbol_short!("adm_rvok"),
            Some(who.clone()),
            Some(symbol_short!("manager")),
            Some(if was_manager { 1 } else { 0 }),
            Some(0),
        )?;
        emit_permission_changed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("manager"),
            &who,
            false,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Updates the max per-transaction withdrawal limit. Admin only.
    pub fn set_withdrawal_limit(env: Env, caller: Address, new_limit: i128) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        if new_limit <= 0 {
            return Err(Error::InvalidArgument);
        }
        let previous = instance_get::<_, i128>(&env, &MAX_WD).unwrap_or(0);
        instance_set(&env, &MAX_WD, &new_limit);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::ConfigChanged,
            symbol_short!("wd_limit"),
            symbol_short!("admin_cfg"),
            None,
            None,
            Some(previous),
            Some(new_limit),
        )?;
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("wd_limit"),
            &caller,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Credits `amount` into `category`'s balance for `token`. `TreasuryManager` only.
    pub fn deposit(
        env: Env,
        caller: Address,
        token: Address,
        category: Symbol,
        amount: i128,
    ) -> Result<(), Error> {
        // Cheap validation first — avoids auth commit on trivial rejects.
        if amount <= 0 {
            return Err(Error::InvalidArgument);
        }
        auth::require_permission(&env, &caller, Permission::TreasuryOperations)?;
        let key = (BALANCE, token.clone(), category.clone());
        let balance: i128 = env.storage().instance().get(&key).unwrap_or(0);
        let new_balance = balance.checked_add(amount).ok_or(Error::Overflow)?;
        token::Client::new(&env, &token).transfer(
            &caller,
            &env.current_contract_address(),
            &amount,
        );
        env.storage().instance().set(&key, &new_balance);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::RecordUpdated,
            symbol_short!("deposit"),
            symbol_short!("funds_in"),
            Some(token.clone()),
            Some(category.clone()),
            Some(balance),
            Some(new_balance),
        )?;
        emit_treasury_deposit(&env, &zero_correlation_id(&env),
            category, &caller, &token, amount, new_balance);
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("deposit"),
            &caller,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Returns the current balance for `token` in `category` (0 if never funded).
    pub fn category_balance(env: Env, token: Address, category: Symbol) -> i128 {
        instance_get::<_, i128>(&env, &(BALANCE, token, category)).unwrap_or(0)
    }

    /// Returns the currently configured max per-transaction withdrawal limit.
    pub fn withdrawal_limit(env: Env) -> i128 {
        instance_get::<_, i128>(&env, &MAX_WD).unwrap_or(0)
    }

    /// Withdraws `amount` of `token` from `category` to `to`.
    ///
    /// Guards, in order:
    /// 1. `amount` must be > 0                           → `Error::InvalidArgument`
    /// 2. `amount` must not exceed the withdrawal limit  → `Error::WithdrawalLimitExceeded`
    /// 3. `amount` must not exceed the category balance  → `Error::InsufficientBalance`
    /// 4. `caller` must hold `TreasuryManager`          → `Error::Unauthorized`
    ///
    /// On success, decrements the category balance and emits the shared
    /// `TreasuryWithdrawal` event with `(category, to, token, amount, remaining)`.
    pub fn withdraw(
        env: Env,
        caller: Address,
        token: Address,
        to: Address,
        amount: i128,
        category: Symbol,
    ) -> Result<(), Error> {
        // Gas optimization: cheap validation checks first
        if amount <= 0 {
            return Err(Error::InvalidArgument);
        }

        let limit: i128 = instance_get(&env, &MAX_WD).unwrap_or(0);
        if amount > limit {
            return Err(Error::WithdrawalLimitExceeded);
        }

        let key = (BALANCE, token.clone(), category.clone());
        let balance: i128 = instance_get(&env, &key).unwrap_or(0);
        if amount > balance {
            return Err(Error::InsufficientBalance);
        }

        // Auth check last
        auth::require_permission(&env, &caller, Permission::TreasuryOperations)?;

        // Quota enforcement: fail-open when unconfigured.
        shared::quota::check_and_consume(&env, &caller, &symbol_short!("wdraw"), amount)?;

        let remaining = balance - amount;
        instance_set(&env, &key, &remaining);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::PaymentSent,
            symbol_short!("withdraw"),
            symbol_short!("funds_out"),
            Some(token.clone()),
            Some(category.clone()),
            Some(balance),
            Some(remaining),
        )?;

        emit_treasury_withdrawal(&env, &zero_correlation_id(&env),
            category, &to, &token, amount, remaining);
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("withdraw"),
            &caller,
            true,
            env.ledger().timestamp(),
        );

        Ok(())
    }

    /// Schedules a delayed withdrawal that may only execute inside `window`.
    ///
    /// The schedule is stored keyed by `action_id` so it can be executed later
    /// via [`TreasuryContract::execute_scheduled_withdraw`]. Admin only.
    pub fn schedule_withdraw(
        env: Env,
        caller: Address,
        action_id: Symbol,
        token: Address,
        to: Address,
        amount: i128,
        category: Symbol,
        window: TimeWindow,
    ) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        if amount <= 0 {
            return Err(Error::InvalidArgument);
        }
        // Validate the window shape up-front so we never persist a degenerate
        // schedule that can never execute.
        window.validate(window.not_before)?;
        let key = (SCHEDULE, action_id.clone());
        if env.storage().instance().has(&key) {
            return Err(Error::InvalidArgument);
        }
        let record = (token.clone(), to.clone(), amount, category.clone(), window.clone());
        env.storage().instance().set(&key, &record);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::ConfigChanged,
            symbol_short!("sched_wd"),
            symbol_short!("admin_cfg"),
            Some(token),
            Some(action_id),
            Some(window.not_before as i128),
            Some(window.expires_at as i128),
        )?;
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("sched_wd"),
            &caller,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Executes a previously scheduled withdrawal, enforcing its time window.
    ///
    /// Rejects with `Error::ActionTooEarly` before `not_before`, and
    /// `Error::ActionExpired` at or after `expires_at`. The schedule is
    /// consumed on success so it cannot be replayed.
    pub fn execute_scheduled_withdraw(
        env: Env,
        caller: Address,
        action_id: Symbol,
    ) -> Result<(), Error> {
        let key = (SCHEDULE, action_id.clone());
        let record: (Address, Address, i128, Symbol, TimeWindow) = env
            .storage()
            .instance()
            .get(&key)
            .ok_or(Error::NotFound)?;
        let (token, to, amount, category, window) = record;

        // Window check before auth so early/late/stale attempts are cheap and
        // produce a precise error even for unauthorised callers.
        validate_window(&env, &caller, symbol_short!("exec_wd"), &window)?;

        auth::require_permission(&env, &caller, Permission::TreasuryOperations)?;

        let limit: i128 = instance_get(&env, &MAX_WD).unwrap_or(0);
        if amount > limit {
            return Err(Error::WithdrawalLimitExceeded);
        }
        let bal_key = (BALANCE, token.clone(), category.clone());
        let balance: i128 = instance_get(&env, &bal_key).unwrap_or(0);
        if amount > balance {
            return Err(Error::InsufficientBalance);
        }
        shared::quota::check_and_consume(&env, &caller, &symbol_short!("wdraw"), amount)?;

        let remaining = balance - amount;
        instance_set(&env, &bal_key, &remaining);
        env.storage().instance().remove(&key);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::PaymentSent,
            symbol_short!("exec_wd"),
            symbol_short!("schd_exec"),
            Some(token.clone()),
            Some(category.clone()),
            Some(balance),
            Some(remaining),
        )?;
        emit_treasury_withdrawal(&env, &zero_correlation_id(&env),
            category, &to, &token, amount, remaining);
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("exec_wd"),
            &caller,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Returns the stored time window for `action_id`, if a schedule exists.
    pub fn scheduled_window(env: Env, action_id: Symbol) -> Option<TimeWindow> {
        let key = (SCHEDULE, action_id);
        env.storage()
            .instance()
            .get::<_, (Address, Address, i128, Symbol, TimeWindow)>(&key)
            .map(|(_, _, _, _, w)| w)
    }

    /// Emergency reserve withdrawal for `token`.
    ///
    /// Only callable by the admin, and only while the contract is paused.
    /// Intended to move reserve funds to safety when something has gone wrong.
    pub fn emergency_withdraw(
        env: Env,
        caller: Address,
        token: Address,
        to: Address,
        amount: i128,
    ) -> Result<(), Error> {
        // Gas optimization: cheapest validations first.
        if amount <= 0 {
            return Err(Error::InvalidArgument);
        }
        if !shared::storage::is_paused(&env) {
            return Err(Error::NotPaused);
        }

        let key = (BALANCE, token.clone(), RESERVE_CATEGORY);
        let balance: i128 = instance_get(&env, &key).unwrap_or(0);
        let new_balance = balance
            .checked_sub(amount)
            .ok_or(Error::InsufficientBalance)?;
        if new_balance < 0 {
            return Err(Error::InsufficientBalance);
        }

        // Auth check last
        auth::require_admin(&env, &caller)?;
        instance_set(&env, &key, &new_balance);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::PaymentSent,
            symbol_short!("emrg_wd"),
            symbol_short!("emergency"),
            Some(token.clone()),
            Some(RESERVE_CATEGORY),
            Some(balance),
            Some(new_balance),
        )?;

        events::emit(
            &env,
            events::TREASURY_EMERGENCY_WITHDRAW,
            (caller.clone(), token, to, amount),
        );
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("emrg_wd"),
            &caller,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Registers the referral contract address authorised to call
    /// `distribute_reward`. Admin only.
    pub fn set_referral_contract(
        env: Env,
        caller: Address,
        referral_contract: Address,
    ) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        let previous = instance_get::<_, Address>(&env, &REFERRAL_CONTRACT);
        let was_configured = previous.is_some();
        if let Some(previous_contract) = previous {
            auth::revoke_role(&env, &caller, &previous_contract, Role::ServiceActor)?;
        }
        auth::grant_role(&env, &caller, &referral_contract, Role::ServiceActor)?;
        instance_set(&env, &REFERRAL_CONTRACT, &referral_contract);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::ConfigChanged,
            symbol_short!("ref_ctr"),
            symbol_short!("admin_cfg"),
            Some(referral_contract.clone()),
            None,
            Some(if was_configured { 1 } else { 0 }),
            Some(1),
        )?;
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("ref_ctr"),
            &caller,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Returns the currently registered referral contract address, if any.
    pub fn referral_contract(env: Env) -> Option<Address> {
        instance_get(&env, &REFERRAL_CONTRACT)
    }

    /// Pays a referral commission of `amount` in `token` to `recipient` from the
    /// `Rewards` category.
    ///
    /// Callable only by the registered referral contract.
    pub fn distribute_reward(
        env: Env,
        token: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<(), Error> {
        // Cheap validation before auth commit.
        if amount <= 0 {
            return Err(Error::InvalidArgument);
        }

        let key = (BALANCE, token.clone(), REWARDS_CATEGORY);
        let balance: i128 = instance_get(&env, &key).unwrap_or(0);
        if amount > balance {
            return Err(Error::InsufficientBalance);
        }

        // Auth check after cheap validations pass.
        let referral_contract: Address =
            instance_get(&env, &REFERRAL_CONTRACT).ok_or(Error::Unauthorized)?;
        auth::require_permission(&env, &referral_contract, Permission::ServiceOperation)?;

        let remaining = balance - amount;
        instance_set(&env, &key, &remaining);
        record_treasury_audit(
            &env,
            &referral_contract,
            TimelineEventType::PaymentSent,
            symbol_short!("reward"),
            symbol_short!("ref_pay"),
            Some(token.clone()),
            Some(REWARDS_CATEGORY),
            Some(balance),
            Some(remaining),
        )?;

        emit_commission_paid(&env, &zero_correlation_id(&env),
            &recipient, &token, amount, env.ledger().timestamp());
        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("treasury"),
            symbol_short!("reward"),
            &referral_contract,
            true,
            env.ledger().timestamp(),
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Quota management (Issue #65) — maintainer diagnostics & overrides
    // -----------------------------------------------------------------------

    /// Set quota limits for a resource (admin only).
    pub fn set_quota_config(
        env: Env,
        caller: Address,
        resource: Symbol,
        config: shared::quota::QuotaConfig,
    ) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        shared::quota::set_quota_config(&env, &resource, &config)?;
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::ConfigChanged,
            symbol_short!("quota_cfg"),
            symbol_short!("admin_cfg"),
            None,
            Some(resource.clone()),
            None,
            None,
        )
    }

    /// Inspect quota usage for an actor/resource pair (maintainer diagnostics).
    pub fn quota_status(env: Env, actor: Address, resource: Symbol) -> shared::quota::QuotaStatus {
        shared::quota::get_quota_status(&env, &actor, &resource)
    }

    /// Returns the newest maintainer-only audit entries.
    pub fn audit_trail(
        env: Env,
        maintainer: Address,
        limit: u32,
    ) -> Result<soroban_sdk::Vec<shared::ActionAuditEntry>, Error> {
        shared::timeline::action_audit_trail(&env, &maintainer, limit)
    }

    /// Reset quota usage for an actor/resource pair (admin override path).
    pub fn reset_quota(
        env: Env,
        caller: Address,
        actor: Address,
        resource: Symbol,
    ) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        shared::quota::reset_quota(&env, &actor, &resource);
        record_treasury_audit(
            &env,
            &caller,
            TimelineEventType::ConfigChanged,
            symbol_short!("quota_rst"),
            symbol_short!("adm_ovr"),
            Some(actor.clone()),
            Some(resource.clone()),
            None,
            None,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod invariants_test;

/// CPU/memory regression suite for the treasury's critical entry points.
/// Kept separate from `test` so the behavioural tests and the budget
/// thresholds can be read (and updated) independently.
#[cfg(test)]
mod budget_test;
