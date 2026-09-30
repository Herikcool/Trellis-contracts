//! # Example Payments Contract
//!
//! Demonstrates how to use the shared [`shared::payments`] module to build a
//! full-featured payment gateway with:
//!
//! - **Token deposits** — users deposit SPL / SAC tokens into the contract.
//! - **Escrow** — funds are held until a release or refund condition is met.
//! - **Pull-based withdrawals** — beneficiaries trigger their own payouts.
//! - **Batch payments** — a single call distributes to many recipients.
//! - **Fee handling** — configurable basis-point fee on transfers.
//!
//! This contract is intentionally kept thin: it wires the shared payment
//! primitives together with basic role checks (admin only) so that the
//! payment module's API is the main focus.

#![no_std]

use soroban_sdk::BytesN;
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol};

use shared::auth;
use shared::errors::Error;
use shared::events::emit_action_executed;
use shared::payments::{
    self, create_escrow, get_escrow, refund_escrow, release_escrow, safe_transfer_from_contract,
    EscrowRecord,
};
use shared::storage::{instance_get, instance_set};

// ===========================================================================
// Storage keys
// ===========================================================================

const TOKEN: Symbol = symbol_short!("token");
const FEE_RATE: Symbol = symbol_short!("fee_rt");
const FEE_RECIPIENT: Symbol = symbol_short!("fee_rcp");
const DEPOSITED: Symbol = symbol_short!("depos");
const TOTAL_DEPOSITS: Symbol = symbol_short!("t_depo");
const SUPPORTED_TOKEN: Symbol = symbol_short!("sup_tok");

// ===========================================================================
// Types
// ===========================================================================

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentConfig {
    pub admin: Address,
    pub token: Address,
    pub fee_rate_bps: i128,
    pub fee_recipient: Address,
}

// ===========================================================================
// Contract
// ===========================================================================

fn zero_correlation_id(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0; 32])
}

#[contract]
pub struct ExamplePaymentsContract;

#[contractimpl]
impl ExamplePaymentsContract {
    // -----------------------------------------------------------------------
    // Initialisation
    // -----------------------------------------------------------------------

    /// Initialise the payment gateway.
    ///
    /// Must be called once immediately after deployment.
    pub fn initialize(
        env: Env,
        admin: Address,
        token: Address,
        fee_rate_bps: i128,
        fee_recipient: Address,
    ) -> Result<(), Error> {
        if !(0..=10_000).contains(&fee_rate_bps) {
            return Err(Error::PaymentInvalidFeeRate);
        }

        auth::initialize_admin(&env, &admin)?;
        instance_set(&env, &TOKEN, &token);
        instance_set(&env, &(SUPPORTED_TOKEN, token.clone()), &true);
        instance_set(&env, &FEE_RATE, &fee_rate_bps);
        instance_set(&env, &FEE_RECIPIENT, &fee_recipient);
        instance_set(&env, &TOTAL_DEPOSITS, &0_i128);

        shared::events::emit_module_initialized(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("pay_gw"),
            1,
            &admin,
            env.ledger().timestamp(),
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Configuration (admin only)
    // -----------------------------------------------------------------------

    /// Update the fee rate. Admin only.
    pub fn set_fee_rate(env: Env, caller: Address, new_rate_bps: i128) -> Result<(), Error> {
        if !(0..=10_000).contains(&new_rate_bps) {
            return Err(Error::PaymentInvalidFeeRate);
        }
        auth::require_admin(&env, &caller)?;
        instance_set(&env, &FEE_RATE, &new_rate_bps);

        emit_action_executed(
            &env,
            &zero_correlation_id(&env),
            symbol_short!("pay_gw"),
            symbol_short!("fee_set"),
            &caller,
            true,
            env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Returns the current fee rate in basis points.
    pub fn fee_rate(env: Env) -> i128 {
        instance_get(&env, &FEE_RATE).unwrap_or(0)
    }

    /// Returns the configured fee recipient address.
    pub fn fee_recipient(env: Env) -> Option<Address> {
        instance_get(&env, &FEE_RECIPIENT)
    }

    /// Add a token to the supported-token whitelist. Admin only.
    pub fn add_supported_token(env: Env, caller: Address, token: Address) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        instance_set(&env, &(SUPPORTED_TOKEN, token), &true);
        Ok(())
    }

    /// Remove a token from the supported-token whitelist. Admin only.
    pub fn remove_supported_token(env: Env, caller: Address, token: Address) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        instance_set(&env, &(SUPPORTED_TOKEN, token), &false);
        Ok(())
    }

    /// Returns true when a token is whitelisted for deposits and payouts.
    pub fn is_supported_token(env: Env, token: Address) -> bool {
        instance_get(&env, &(SUPPORTED_TOKEN, token)).unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // Deposits
    // -----------------------------------------------------------------------

    /// Deposit tokens into the contract.
    ///
    /// The caller must have pre-authorised this contract.  The deposited
    /// amount is tracked and credited to the caller's internal balance.
    pub fn deposit(env: Env, from: Address, token: Address, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::PaymentInvalidAmount);
        }
        require_supported_token(&env, &token)?;

        // Transfer tokens from the depositor into this contract.
        let deposit_client = soroban_sdk::token::Client::new(&env, &token);
        from.require_auth();
        deposit_client.transfer(&from, &env.current_contract_address(), &amount);

        // Update internal accounting.
        let key = (DEPOSITED, from.clone(), token.clone());
        let balance: i128 = instance_get(&env, &key).unwrap_or(0);
        let new_balance = balance.checked_add(amount).ok_or(Error::Overflow)?;
        instance_set(&env, &key, &new_balance);

        let total_key = (TOTAL_DEPOSITS, token.clone());
        let total: i128 = instance_get(&env, &total_key).unwrap_or(0);
        instance_set(
            &env,
            &total_key,
            &total.checked_add(amount).ok_or(Error::Overflow)?,
        );

        shared::events::emit(
            &env,
            shared::TREASURY_DEPOSIT,
            (symbol_short!("token"), from, token, amount, new_balance),
        );

        Ok(())
    }

    /// Returns the internal balance for `who`.
    pub fn balance_of(env: Env, who: Address, token: Address) -> i128 {
        instance_get(&env, &(DEPOSITED, who, token)).unwrap_or(0)
    }

    /// Returns total deposits across all users for a token.
    pub fn total_deposits(env: Env, token: Address) -> i128 {
        instance_get(&env, &(TOTAL_DEPOSITS, token)).unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // Withdrawals (pull-based payouts with optional fee)
    // -----------------------------------------------------------------------

    /// Withdraw the caller's full deposited balance, deducting a fee.
    ///
    /// The contract holds the tokens, so the fee and net transfers are both
    /// pull-based: the contract sends the fee to `fee_recipient` and the
    /// net to `who`.
    pub fn withdraw(env: Env, who: Address, token: Address) -> Result<i128, Error> {
        who.require_auth();
        require_supported_token(&env, &token)?;

        let deposited: i128 =
            instance_get(&env, &(DEPOSITED, who.clone(), token.clone())).unwrap_or(0);
        if deposited <= 0 {
            return Err(Error::PaymentInsufficientBalance);
        }

        let fee_rate: i128 = instance_get(&env, &FEE_RATE).unwrap_or(0);
        let fee_recipient_addr: Address =
            instance_get(&env, &FEE_RECIPIENT).ok_or(Error::NotFound)?;

        // Calculate fee and net (pure math, no transfer).
        let (fee, net) = payments::calculate_fee_split(&env, deposited, fee_rate)?;

        // Transfer fee from contract to fee recipient.
        if fee > 0 {
            safe_transfer_from_contract(&env, &token, &fee_recipient_addr, fee)?;
        }

        // Transfer net from contract to withdrawer.
        if net > 0 {
            safe_transfer_from_contract(&env, &token, &who, net)?;
        }

        // Clear the depositor's internal balance.
        instance_set(&env, &(DEPOSITED, who.clone(), token.clone()), &0_i128);

        // Decrement total deposits.
        let total_key = (TOTAL_DEPOSITS, token.clone());
        let total: i128 = instance_get(&env, &total_key).unwrap_or(0);
        instance_set(&env, &total_key, &total.checked_sub(deposited).unwrap_or(0));

        shared::events::emit(
            &env,
            shared::TREASURY_WITHDRAW,
            (symbol_short!("token"), who, token, deposited, net),
        );

        Ok(net)
    }

    /// Withdraw a specific amount, deducting a fee.
    pub fn withdraw_amount(
        env: Env,
        who: Address,
        token: Address,
        amount: i128,
    ) -> Result<i128, Error> {
        if amount <= 0 {
            return Err(Error::PaymentInvalidAmount);
        }
        who.require_auth();
        require_supported_token(&env, &token)?;

        let deposited: i128 =
            instance_get(&env, &(DEPOSITED, who.clone(), token.clone())).unwrap_or(0);
        if amount > deposited {
            return Err(Error::PaymentInsufficientBalance);
        }

        let fee_rate: i128 = instance_get(&env, &FEE_RATE).unwrap_or(0);
        let fee_recipient_addr: Address =
            instance_get(&env, &FEE_RECIPIENT).ok_or(Error::NotFound)?;

        // Calculate fee and net (pure math, no transfer).
        let (fee, net) = payments::calculate_fee_split(&env, amount, fee_rate)?;

        // Transfer fee from contract to fee recipient.
        if fee > 0 {
            safe_transfer_from_contract(&env, &token, &fee_recipient_addr, fee)?;
        }

        // Transfer net from contract to withdrawer.
        if net > 0 {
            safe_transfer_from_contract(&env, &token, &who, net)?;
        }

        // Decrement internal balance.
        let new_balance = deposited - amount;
        instance_set(&env, &(DEPOSITED, who.clone(), token.clone()), &new_balance);

        // Decrement total deposits.
        let total_key = (TOTAL_DEPOSITS, token.clone());
        let total: i128 = instance_get(&env, &total_key).unwrap_or(0);
        instance_set(&env, &total_key, &total.checked_sub(amount).unwrap_or(0));

        Ok(net)
    }

    // -----------------------------------------------------------------------
    // Escrow
    // -----------------------------------------------------------------------

    /// Create an escrow deposit from `depositor` to `beneficiary`.
    ///
    /// Funds are held until `release_escrow_entry` or `refund_escrow_entry`
    /// is called.
    pub fn create_escrow_entry(
        env: Env,
        depositor: Address,
        beneficiary: Address,
        token: Address,
        amount: i128,
        expiry_ledger: u32,
    ) -> Result<u64, Error> {
        depositor.require_auth();
        require_supported_token(&env, &token)?;

        create_escrow(
            &env,
            &token,
            &depositor,
            &beneficiary,
            amount,
            expiry_ledger,
        )
    }

    /// Release an escrow deposit to the beneficiary. Admin only.
    pub fn release_escrow_entry(env: Env, caller: Address, escrow_id: u64) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        let token = get_escrow(&env, escrow_id)
            .ok_or(Error::PaymentEscrowNotFound)?
            .token;

        release_escrow(&env, &token, escrow_id)
    }

    /// Refund an escrow deposit back to the depositor. Admin only.
    pub fn refund_escrow_entry(env: Env, caller: Address, escrow_id: u64) -> Result<(), Error> {
        auth::require_admin(&env, &caller)?;
        let token = get_escrow(&env, escrow_id)
            .ok_or(Error::PaymentEscrowNotFound)?
            .token;

        refund_escrow(&env, &token, escrow_id)
    }

    /// Read an escrow record.
    pub fn get_escrow_entry(env: Env, escrow_id: u64) -> Option<EscrowRecord> {
        get_escrow(&env, escrow_id)
    }

    // -----------------------------------------------------------------------
    // Batch payouts
    // -----------------------------------------------------------------------

    /// Distribute tokens from the contract's own balance to multiple
    /// recipients. Admin only.
    ///
    /// Uses the shared `multi_transfer_all` batch executor with atomic mode.
    pub fn batch_payout(
        env: Env,
        caller: Address,
        token: Address,
        recipients: soroban_sdk::Vec<(Address, i128)>,
    ) -> shared::batch::BatchResult {
        auth::require_admin(&env, &caller).unwrap();
        require_supported_token(&env, &token).unwrap();
        let contract_addr = env.current_contract_address();

        // Build the Rust slice from the Soroban Vec
        extern crate std;
        let mut rust_vec = std::vec::Vec::new();
        for r in recipients.iter() {
            rust_vec.push(r);
        }

        shared::payments::multi_transfer_all(&env, &contract_addr, &token, &rust_vec).unwrap()
    }

    /// Returns the configured token address.
    pub fn get_token(env: Env) -> Option<Address> {
        instance_get(&env, &TOKEN)
    }
}

fn require_supported_token(env: &Env, token: &Address) -> Result<(), Error> {
    if instance_get(env, &(SUPPORTED_TOKEN, token.clone())).unwrap_or(false) {
        Ok(())
    } else {
        Err(Error::PaymentTokenNotSupported)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod invariants_test;
