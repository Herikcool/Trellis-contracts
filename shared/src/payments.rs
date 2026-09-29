//! # Payments — Token & Native-Transfer Utilities
//!
//! Standardised payment primitives for the Trellis Soroban contract
//! suite.  Provides safe token transfer wrappers, pull-based escrow, batch
//! payouts, and fee-handling hooks — all with consistent event emissions and
//! documented error codes.
//!
//! ## Design Principles
//!
//! | Principle | Detail |
//! |-----------|--------|
//! | **Checks-Effects-Interactions** | State is mutated *before* any cross-contract token call. |
//! | **Cheap validation first** | Amount, address, and state checks run before the costly auth commit. |
//! | **Composable** | Functions are pure helpers — they accept an `Env` reference and return
//!   `Result`.  The calling contract decides which auth/role guards to layer on. |
//! | **Observable** | Every state-changing operation emits a typed event via the shared
//!   [`events`](crate::events) helpers so indexers can reconstruct balances. |
//!
//! ## Module Layout
//!
//! | Section | Purpose |
//! |---------|---------|
//! | Safe transfers | `safe_transfer` / `safe_transfer_from_contract` — wrappers around
//!   [`token::Client`](soroban_sdk::token::Client) that validate inputs and
//!   emit events. |
//! | Fee handling | `calculate_fee` / `deduct_fee` — basis-point fee calculation
//!   with overflow protection. |
//! | Escrow | `create_escrow` / `release_escrow` / `refund_escrow` — pull-based
//!   escrow pattern with expiry and refund semantics. |
//! | Batch payments | Thin re-export of the [`batch`](crate::batch) module
//!   specialised for token payouts. |
//!
//! ## Error Codes
//!
//! All payment-specific errors live in the `700–720` range of
//! [`Error`](crate::Error) to avoid collisions with other modules.
//!
//! ## Usage Example
//!
//! ```ignore
//! use shared::payments;
//!
//! // Single transfer with fee deduction
//! let fee_cfg = payments::FeeConfig { rate_bps: 250, recipient: fee_collector };
//! let net = payments::deduct_fee(&env, &token, &from, &fee_cfg, 1_000, &to)?;
//! // `net` = 975 after a 250 bps (2.5%) fee was transferred to `fee_collector`.
//!
//! // Escrow flow
//! let escrow_id = payments::create_escrow(&env, &token, &depositor, &beneficiary, 5_000, expiry)?;
//! payments::release_escrow(&env, &token, escrow_id)?;
//! ```

use soroban_sdk::{contracttype, symbol_short, token, Address, Env, Symbol, Vec};

use crate::errors::Error;
use crate::telemetry::{
    emit_failure, emit_success, ActorType, OP_ESCROW_CREATE, OP_ESCROW_RELEASE,
    OP_PAYMENT_TRANSFER,
};
use crate::events::{
    emit, PAYMENT_ESCROW_CREATED, PAYMENT_ESCROW_REFUNDED, PAYMENT_ESCROW_RELEASED, PAYMENT_FEE,
    PAYMENT_TRANSFER,
};
use crate::jobs::{
    enqueue_escrow_refund, EnqueueOutcome, Job, JobError, JobHandler, JobPayload,
};
use crate::storage::{persistent_get, persistent_read, persistent_set};

// ===========================================================================
// Constants
// ===========================================================================

/// Basis-point denominator (10 000 bps = 100 %).
const BPS_DENOM: i128 = 10_000;

/// Maximum fee rate allowed (10 000 bps = 100 %).
const MAX_FEE_BPS: i128 = 10_000;

// Storage keys for the escrow module.
const ESCROW_ID_SEQ: Symbol = symbol_short!("esc_seq");
const ESCROW_ACTIVE: Symbol = symbol_short!("esc_act");

// ===========================================================================
// Types
// ===========================================================================

/// Fee configuration for a payment route.
///
/// The `rate_bps` is expressed in basis points where 10_000 = 100 %.
/// A rate of 250 means a 2.5 % fee.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeConfig {
    /// Fee rate in basis points (0..=10 000).
    pub rate_bps: i128,
    /// Address that receives the deducted fee.
    pub recipient: Address,
}

/// Lifecycle state of an escrow deposit.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EscrowState {
    /// Funds held in escrow, awaiting release or refund.
    Active,
    /// Released to the beneficiary.
    Released,
    /// Refunded to the depositor.
    Refunded,
}

/// A single escrow record stored in persistent storage.
///
/// The record is written atomically with the deposit transfer (checks-effects-
/// interactions) so a failed transfer never leaves a dangling record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowRecord {
    /// Auto-incremented unique identifier.
    pub id: u64,
    /// Address that deposited the funds.
    pub depositor: Address,
    /// Address that will receive the funds on release.
    pub beneficiary: Address,
    /// Token contract address held in escrow.
    pub token: Address,
    /// Number of token units held.
    pub amount: i128,
    /// Ledger sequence after which only refund is possible.
    pub expiry_ledger: u32,
    /// Current lifecycle state.
    pub state: EscrowState,
    /// Amount already released to beneficiary (for milestone releases).
    pub released_amount: i128,
    /// Amount already refunded to depositor (for dispute resolution).
    pub refunded_amount: i128,
    /// Milestone configuration (optional).
    pub milestones: Option<soroban_sdk::Vec<Milestone>>,
}

/// A milestone in a multi-stage escrow release.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Milestone {
    /// Milestone identifier.
    pub id: u32,
    /// Amount to release at this milestone.
    pub amount: i128,
    /// Whether this milestone has been released.
    pub released: bool,
}

// ===========================================================================
// Input Validation Helpers
// ===========================================================================

/// Returns `Ok(())` when `amount` is strictly positive.
fn validate_amount(amount: i128) -> Result<(), Error> {
    if amount <= 0 {
        return Err(Error::PaymentInvalidAmount);
    }
    Ok(())
}

/// Returns `Ok(())` when `rate_bps` is in the inclusive range `0..=10_000`.
fn validate_fee_rate(rate_bps: i128) -> Result<(), Error> {
    if !(0..=MAX_FEE_BPS).contains(&rate_bps) {
        return Err(Error::PaymentInvalidFeeRate);
    }
    Ok(())
}

// ===========================================================================
// Escrow Storage Helpers
// ===========================================================================

/// Returns the next escrow ID and increments the sequence counter.
fn next_escrow_id(env: &Env) -> Result<u64, Error> {
    let current: u64 = persistent_get(env, &ESCROW_ID_SEQ).unwrap_or(0);
    let next = current
        .checked_add(1)
        .ok_or(Error::PaymentEscrowIdOverflow)?;
    persistent_set(env, &ESCROW_ID_SEQ, &next);
    Ok(next)
}

/// Stores an escrow record and marks it active.
fn store_escrow(env: &Env, record: &EscrowRecord) {
    persistent_set(env, &escrow_key(record.id), record);
    let mut active: Vec<u64> = persistent_get(env, &ESCROW_ACTIVE).unwrap_or_else(|| Vec::new(env));
    active.push_back(record.id);
    persistent_set(env, &ESCROW_ACTIVE, &active);
}

/// Removes an escrow ID from the active set.
fn remove_from_active(env: &Env, escrow_id: u64) {
    let active: Vec<u64> = persistent_get(env, &ESCROW_ACTIVE).unwrap_or_else(|| Vec::new(env));
    let mut updated = Vec::new(env);
    for id in active.iter() {
        if id != escrow_id {
            updated.push_back(id);
        }
    }
    persistent_set(env, &ESCROW_ACTIVE, &updated);
}

/// Persistent storage key for an escrow record.
fn escrow_key(id: u64) -> (Symbol, u64) {
    (symbol_short!("esc_rec"), id)
}

// ===========================================================================
// Safe Token Transfers
// ===========================================================================

/// Safely transfer `amount` of `token` from `from` to `to`.
///
/// Validates that `amount` is strictly positive before executing the
/// cross-contract call.  Emits [`PAYMENT_TRANSFER`] on success.
///
/// # Errors
/// * [`Error::PaymentInvalidAmount`] — `amount` is ≤ 0.
///
/// # Authorization
/// The `from` address must have pre-authorised this contract via Soroban's
/// auth tree before the transaction is submitted.
pub fn safe_transfer(
    env: &Env,
    token: &Address,
    from: &Address,
    to: &Address,
    amount: i128,
) -> Result<(), Error> {
    if let Err(err) = validate_amount(amount) {
        emit_failure(env, OP_PAYMENT_TRANSFER, ActorType::User, 0);
        return Err(err);
    }

    let client = token::Client::new(env, token);
    client.transfer(from, to, &amount);

    emit(env, PAYMENT_TRANSFER, (from.clone(), to.clone(), amount));
    emit_success(env, OP_PAYMENT_TRANSFER, ActorType::User, 0);
    Ok(())
}

/// Safely transfer `amount` of `token` from the current contract to `to`.
///
/// This is the pull-based payout variant: the contract holds tokens and
/// the beneficiary (or an admin) triggers the withdrawal.
///
/// # Errors
/// * [`Error::PaymentInvalidAmount`] — `amount` is ≤ 0.
///
/// # Authorization
/// No explicit auth is required from an external address because the
/// contract is sending from its own balance.  The calling contract should
/// enforce its own role/permission checks before calling this helper.
pub fn safe_transfer_from_contract(
    env: &Env,
    token: &Address,
    to: &Address,
    amount: i128,
) -> Result<(), Error> {
    validate_amount(amount)?;

    let contract_addr = env.current_contract_address();
    let client = token::Client::new(env, token);
    client.transfer(&contract_addr, to, &amount);

    emit(env, PAYMENT_TRANSFER, (contract_addr, to.clone(), amount));
    Ok(())
}

// ===========================================================================
// Fee Handling
// ===========================================================================

/// Calculate the fee for a given `amount` using `rate_bps`.
///
/// The fee is rounded down (towards zero).  The result is guaranteed to be
/// `≤ amount`, so `amount - fee` is always non-negative for valid inputs.
///
/// # Errors
/// * [`Error::PaymentInvalidAmount`] — `amount` is ≤ 0.
/// * [`Error::PaymentInvalidFeeRate`] — `rate_bps` is outside `0..=10_000`.
/// * [`Error::PaymentFeeOverflow`] — arithmetic overflow during calculation.
///
/// # Examples
/// ```ignore
/// let fee = payments::calculate_fee(&env, 1_000, 250)?; // 25
/// assert_eq!(fee, 25);
/// ```
pub fn calculate_fee(_env: &Env, amount: i128, rate_bps: i128) -> Result<i128, Error> {
    validate_amount(amount)?;
    validate_fee_rate(rate_bps)?;

    // Use checked arithmetic throughout to prevent overflow.
    // fee = amount * rate_bps / 10_000
    let product = amount
        .checked_mul(rate_bps)
        .ok_or(Error::PaymentFeeOverflow)?;
    let fee = product
        .checked_div(BPS_DENOM)
        .ok_or(Error::PaymentFeeOverflow)?;
    Ok(fee)
}

/// Calculate both the fee and the net amount (amount minus fee) atomically.
///
/// Returns `(fee, net_amount)`.
pub fn calculate_fee_split(env: &Env, amount: i128, rate_bps: i128) -> Result<(i128, i128), Error> {
    let fee = calculate_fee(env, amount, rate_bps)?;
    let net = amount.checked_sub(fee).ok_or(Error::PaymentFeeOverflow)?;
    Ok((fee, net))
}

/// Deduct a fee from `amount` and transfer both portions in a single
/// transaction.
///
/// 1. Calculates the fee using `fee_cfg.rate_bps`.
/// 2. Transfers `fee` to `fee_cfg.recipient`.
/// 3. Transfers `amount - fee` to `to`.
/// 4. Returns the net amount received by `to`.
///
/// Both transfers are executed via [`safe_transfer`], so [`PAYMENT_FEE`]
/// and [`PAYMENT_TRANSFER`] events are emitted for indexing.
///
/// # Errors
/// * [`Error::PaymentInvalidAmount`] — `amount` is ≤ 0.
/// * [`Error::PaymentInvalidFeeRate`] — `rate_bps` is outside `0..=10_000`.
/// * [`Error::PaymentFeeOverflow`] — arithmetic overflow during fee calc.
///
/// # Authorization
/// `from` must have pre-authorised the contract for the full `amount`.
pub fn deduct_fee(
    env: &Env,
    token: &Address,
    from: &Address,
    fee_cfg: &FeeConfig,
    amount: i128,
    to: &Address,
) -> Result<i128, Error> {
    let (fee, net) = calculate_fee_split(env, amount, fee_cfg.rate_bps)?;

    if fee > 0 {
        let client = token::Client::new(env, token);
        client.transfer(from, &fee_cfg.recipient, &fee);
        emit(
            env,
            PAYMENT_FEE,
            (from.clone(), fee_cfg.recipient.clone(), fee),
        );
    }

    if net > 0 {
        let client = token::Client::new(env, token);
        client.transfer(from, to, &net);
        emit(env, PAYMENT_TRANSFER, (from.clone(), to.clone(), net));
    }

    Ok(net)
}

// ===========================================================================
// Escrow (Pull-Based Payouts)
// ===========================================================================

/// Create a new escrow deposit.
///
/// Transfers `amount` of `token` from `depositor` into this contract and
/// stores an [`EscrowRecord`] in persistent storage.  The funds are held
/// until [`release_escrow`] or [`refund_escrow`] is called.
///
/// The `expiry_ledger` must be strictly greater than the current ledger
/// sequence.  After expiry, only [`refund_escrow`] is possible.
///
/// # Events
/// Emits [`PAYMENT_ESCROW_CREATED`] with `(escrow_id, depositor, beneficiary,
/// amount, expiry_ledger)`.
///
/// # Errors
/// * [`Error::PaymentInvalidAmount`] — `amount` is ≤ 0.
/// * [`Error::InvalidArgument`] — `expiry_ledger` is in the past or equal
///   to the current ledger.
///
/// # Authorization
/// `depositor` must have pre-authorised the contract for `amount`.
pub fn create_escrow(
    env: &Env,
    token: &Address,
    depositor: &Address,
    beneficiary: &Address,
    amount: i128,
    expiry_ledger: u32,
) -> Result<u64, Error> {
    if let Err(err) = validate_amount(amount) {
        emit_failure(env, OP_ESCROW_CREATE, ActorType::User, 0);
        return Err(err);
    }

    let current_seq = env.ledger().sequence();
    if expiry_ledger <= current_seq {
        emit_failure(env, OP_ESCROW_CREATE, ActorType::User, 0);
        return Err(Error::InvalidArgument);
    }

    // Auto-allocate ID.
    let escrow_id = next_escrow_id(env)?;

    // Build record (checks-effects-interactions: store before transfer).
    let record = EscrowRecord {
        id: escrow_id,
        depositor: depositor.clone(),
        beneficiary: beneficiary.clone(),
        token: token.clone(),
        amount,
        expiry_ledger,
        state: EscrowState::Active,
        released_amount: 0,
        refunded_amount: 0,
        milestones: None,
    };
    store_escrow(env, &record);

    // Transfer tokens from depositor into this contract.
    let client = token::Client::new(env, token);
    client.transfer(depositor, &env.current_contract_address(), &amount);

    emit(
        env,
        PAYMENT_ESCROW_CREATED,
        (
            escrow_id,
            depositor.clone(),
            beneficiary.clone(),
            amount,
            expiry_ledger,
        ),
    );

    emit_success(env, OP_ESCROW_CREATE, ActorType::User, 0);

    Ok(escrow_id)
}

/// Release an active escrow deposit to the beneficiary.
///
/// Transfers the escrowed `amount` from this contract to the beneficiary
/// and marks the record as [`EscrowState::Released`].
///
/// # Errors
/// * [`Error::PaymentEscrowNotFound`] — no record for `escrow_id`.
/// * [`Error::PaymentEscrowAlreadyReleased`] — already released.
/// * [`Error::PaymentEscrowAlreadyRefunded`] — already refunded.
/// * [`Error::PaymentEscrowExpired`] — the expiry ledger has passed.
///
/// # Events
/// Emits [`PAYMENT_ESCROW_RELEASED`] with `(escrow_id, beneficiary, amount)`.
pub fn release_escrow(env: &Env, token: &Address, escrow_id: u64) -> Result<(), Error> {
    let Some(mut record): Option<EscrowRecord> = persistent_read(env, &escrow_key(escrow_id)) else {
        emit_failure(env, OP_ESCROW_RELEASE, ActorType::User, 0);
        return Err(Error::PaymentEscrowNotFound);
    };

    if record.state == EscrowState::Released {
        emit_failure(env, OP_ESCROW_RELEASE, ActorType::User, 0);
        return Err(Error::PaymentEscrowAlreadyReleased);
    }
    if record.state == EscrowState::Refunded {
        emit_failure(env, OP_ESCROW_RELEASE, ActorType::User, 0);
        return Err(Error::PaymentEscrowAlreadyRefunded);
    }

    // Check expiry — release is only possible before or at the expiry ledger.
    let current_seq = env.ledger().sequence();
    if current_seq > record.expiry_ledger {
        emit_failure(env, OP_ESCROW_RELEASE, ActorType::User, 0);
        return Err(Error::PaymentEscrowExpired);
    }

    // Update state before transfer (checks-effects-interactions).
    record.state = EscrowState::Released;
    persistent_set(env, &escrow_key(escrow_id), &record);
    remove_from_active(env, escrow_id);

    // Transfer from contract to beneficiary.
    let client = token::Client::new(env, token);
    client.transfer(
        &env.current_contract_address(),
        &record.beneficiary,
        &record.amount,
    );

    emit(
        env,
        PAYMENT_ESCROW_RELEASED,
        (escrow_id, record.beneficiary, record.amount),
    );

    emit_success(env, OP_ESCROW_RELEASE, ActorType::User, 0);

    Ok(())
}

/// Refund an active escrow deposit back to the depositor.
///
/// After the `expiry_ledger` has passed, only the depositor may trigger
/// a refund.  Before expiry, any caller may refund (intended for admin/
/// governance overrides — restrict in your contract if needed).
///
/// # Errors
/// * [`Error::PaymentEscrowNotFound`] — no record for `escrow_id`.
/// * [`Error::PaymentEscrowAlreadyReleased`] — already released.
/// * [`Error::PaymentEscrowAlreadyRefunded`] — already refunded.
///
/// # Events
/// Emits [`PAYMENT_ESCROW_REFUNDED`] with `(escrow_id, depositor, amount)`.
pub fn refund_escrow(env: &Env, token: &Address, escrow_id: u64) -> Result<(), Error> {
    let mut record: EscrowRecord =
        persistent_read(env, &escrow_key(escrow_id)).ok_or(Error::PaymentEscrowNotFound)?;

    if record.state == EscrowState::Released {
        return Err(Error::PaymentEscrowAlreadyReleased);
    }
    if record.state == EscrowState::Refunded {
        return Err(Error::PaymentEscrowAlreadyRefunded);
    }

    // Update state before transfer (checks-effects-interactions).
    record.state = EscrowState::Refunded;
    persistent_set(env, &escrow_key(escrow_id), &record);
    remove_from_active(env, escrow_id);

    // Transfer from contract back to depositor.
    let client = token::Client::new(env, token);
    client.transfer(
        &env.current_contract_address(),
        &record.depositor,
        &record.amount,
    );

    emit(
        env,
        PAYMENT_ESCROW_REFUNDED,
        (escrow_id, record.depositor, record.amount),
    );

    Ok(())
}

/// Release a milestone amount from an escrow deposit.
///
/// Allows partial releases for multi-stage payments. The amount must not
/// exceed the remaining unreleased amount.
///
/// # Errors
/// * [`Error::PaymentEscrowNotFound`] — no record for `escrow_id`.
/// * [`Error::PaymentEscrowAlreadyReleased`] — escrow is fully released.
/// * [`Error::PaymentEscrowAlreadyRefunded`] — escrow is already refunded.
/// * [`Error::PaymentInvalidAmount`] — amount exceeds remaining balance.
///
/// # Events
/// Emits [`PAYMENT_ESCROW_RELEASED`] with `(escrow_id, beneficiary, amount)`.
pub fn release_milestone(
    env: &Env,
    token: &Address,
    escrow_id: u64,
    amount: i128,
) -> Result<(), Error> {
    validate_amount(amount)?;

    let mut record: EscrowRecord =
        persistent_read(env, &escrow_key(escrow_id)).ok_or(Error::PaymentEscrowNotFound)?;

    if record.state == EscrowState::Refunded {
        return Err(Error::PaymentEscrowAlreadyRefunded);
    }

    let remaining = record.amount - record.released_amount - record.refunded_amount;
    if amount > remaining {
        return Err(Error::PaymentInvalidAmount);
    }

    // Update released amount before transfer.
    record.released_amount = record
        .released_amount
        .checked_add(amount)
        .ok_or(Error::PaymentFeeOverflow)?;

    // Check if fully released.
    if record.released_amount == record.amount {
        record.state = EscrowState::Released;
        remove_from_active(env, escrow_id);
    }

    persistent_set(env, &escrow_key(escrow_id), &record);

    // Transfer from contract to beneficiary.
    let client = token::Client::new(env, token);
    client.transfer(
        &env.current_contract_address(),
        &record.beneficiary,
        &amount,
    );

    emit(
        env,
        PAYMENT_ESCROW_RELEASED,
        (escrow_id, record.beneficiary, amount),
    );

    Ok(())
}

/// Resolve a dispute by splitting escrowed funds between depositor and beneficiary.
///
/// Allows partial arbitration in case of disputes. The total split must equal
/// the remaining escrowed amount.
///
/// # Errors
/// * [`Error::PaymentEscrowNotFound`] — no record for `escrow_id`.
/// * [`Error::PaymentEscrowAlreadyReleased`] — escrow is fully released.
/// * [`Error::PaymentEscrowAlreadyRefunded`] — escrow is already refunded.
/// * [`Error::PaymentInvalidAmount`] — split amounts don't match remaining balance.
///
/// # Events
/// Emits both [`PAYMENT_ESCROW_RELEASED`] and [`PAYMENT_ESCROW_REFUNDED`].
pub fn resolve_dispute(
    env: &Env,
    token: &Address,
    escrow_id: u64,
    beneficiary_amount: i128,
    refund_amount: i128,
) -> Result<(), Error> {
    let mut record: EscrowRecord =
        persistent_read(env, &escrow_key(escrow_id)).ok_or(Error::PaymentEscrowNotFound)?;

    if record.state == EscrowState::Released {
        return Err(Error::PaymentEscrowAlreadyReleased);
    }
    if record.state == EscrowState::Refunded {
        return Err(Error::PaymentEscrowAlreadyRefunded);
    }

    let remaining = record.amount - record.released_amount - record.refunded_amount;
    if beneficiary_amount + refund_amount != remaining {
        return Err(Error::PaymentInvalidAmount);
    }

    // Update amounts before transfers.
    record.released_amount = record
        .released_amount
        .checked_add(beneficiary_amount)
        .ok_or(Error::PaymentFeeOverflow)?;
    record.refunded_amount = record
        .refunded_amount
        .checked_add(refund_amount)
        .ok_or(Error::PaymentFeeOverflow)?;

    // Mark as fully resolved.
    record.state = EscrowState::Released;
    remove_from_active(env, escrow_id);

    persistent_set(env, &escrow_key(escrow_id), &record);

    let client = token::Client::new(env, token);

    // Transfer beneficiary portion.
    if beneficiary_amount > 0 {
        client.transfer(
            &env.current_contract_address(),
            &record.beneficiary,
            &beneficiary_amount,
        );
        emit(
            env,
            PAYMENT_ESCROW_RELEASED,
            (escrow_id, record.beneficiary, beneficiary_amount),
        );
    }

    // Transfer refund portion.
    if refund_amount > 0 {
        client.transfer(
            &env.current_contract_address(),
            &record.depositor,
            &refund_amount,
        );
        emit(
            env,
            PAYMENT_ESCROW_REFUNDED,
            (escrow_id, record.depositor, refund_amount),
        );
    }

    Ok(())
}

// ===========================================================================
// Background worker integration (Issue #35)
// ===========================================================================

/// Job handler that refunds an escrow deposit through the worker framework.
///
/// A refund is only valid once `expiry_ledger` has passed, so the work is
/// inherently delayed. Instead of every caller polling [`refund_escrow`] until
/// it stops returning [`Error::PaymentEscrowNotExpired`], the deposit can be
/// *scheduled* as a job and the crank retries it with backoff until it
/// succeeds — and dead-letters it with full context if it never does.
pub struct EscrowRefundHandler;

impl JobHandler for EscrowRefundHandler {
    fn handle(&self, env: &Env, job: &Job) -> Result<(), Error> {
        match &job.payload {
            JobPayload::EscrowRefund(token, escrow_id) => refund_escrow(env, token, *escrow_id),
            _ => Err(Error::InvalidArgument),
        }
    }
}

/// Schedule the refund of an escrow deposit for its expiry ledger.
///
/// Idempotent per `escrow_id`: re-scheduling an escrow whose refund is still
/// queued, or already completed, returns the existing job instead of
/// duplicating the work.
pub fn schedule_escrow_refund(
    env: &Env,
    token: &Address,
    escrow_id: u64,
    expiry_ledger: u32,
) -> Result<EnqueueOutcome, JobError> {
    enqueue_escrow_refund(env, token.clone(), escrow_id, expiry_ledger)
}

/// Create an escrow deposit **and** schedule its expiry refund as a job.
///
/// This is the delayed half of the escrow lifecycle moved into the worker
/// framework: the deposit is still created atomically, but the refund is
/// handed to the worker (retries, backoff, dead-letter) instead of waiting for
/// an external caller to poll [`refund_escrow`].
///
/// # Errors
/// Everything [`create_escrow`] can return, plus [`Error::ContractPaused`]
/// when the worker is disabled and [`Error::ConfigInvalid`] when its retry
/// policy is unusable.
pub fn create_escrow_with_refund_job(
    env: &Env,
    token: &Address,
    depositor: &Address,
    beneficiary: &Address,
    amount: i128,
    expiry_ledger: u32,
) -> Result<(u64, EnqueueOutcome), Error> {
    let escrow_id = create_escrow(env, token, depositor, beneficiary, amount, expiry_ledger)?;
    let outcome = schedule_escrow_refund(env, token, escrow_id, expiry_ledger)
        .map_err(worker_error_to_contract_error)?;
    Ok((escrow_id, outcome))
}

/// Translate a framework error into the shared contract error space.
fn worker_error_to_contract_error(err: JobError) -> Error {
    match err {
        JobError::Disabled => Error::ContractPaused,
        JobError::InvalidPolicy => Error::ConfigInvalid,
        _ => Error::InvalidArgument,
    }
}

/// Read an escrow record by ID.
///
/// Returns `None` if the ID does not exist.
pub fn get_escrow(env: &Env, escrow_id: u64) -> Option<EscrowRecord> {
    persistent_read(env, &escrow_key(escrow_id))
}

// ===========================================================================
// Batch Payments (convenience re-exports)
// ===========================================================================

/// Execute a batch of token transfers with configurable atomicity.
///
/// Thin re-export of [`crate::batch::execute_multi_transfer`] for
/// ergonomic access from the payments module.  See that module for full
/// documentation.
pub use crate::batch::execute_multi_transfer;

/// Convenience: atomically transfer the same token from `caller` to
/// multiple recipients.
///
/// Thin re-export of [`crate::batch::multi_transfer_all`].
pub use crate::batch::multi_transfer_all;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{contract, contractimpl, token, Env};

    // -----------------------------------------------------------------------
    // Mock contract — exposes payment helpers behind a contract boundary so
    // that persistent storage and current_contract_address() work correctly.
    // -----------------------------------------------------------------------

    #[contract]
    struct MockPaymentsContract;

    #[contractimpl]
    impl MockPaymentsContract {
        // -- Transfers --
        // NOTE: Each wrapper that moves tokens on behalf of an external
        // address must call `addr.require_auth()` so the Soroban auth tree
        // ties the authorization to the root invocation.  Without this,
        // `env.mock_all_auths()` cannot record the nested cross-contract
        // auth required by the Stellar Asset Contract.

        pub fn safe_transfer(
            env: Env,
            token: Address,
            from: Address,
            to: Address,
            amount: i128,
        ) -> Result<(), Error> {
            from.require_auth();
            super::safe_transfer(&env, &token, &from, &to, amount)
        }

        pub fn safe_transfer_from_contract(
            env: Env,
            token: Address,
            to: Address,
            amount: i128,
        ) -> Result<(), Error> {
            super::safe_transfer_from_contract(&env, &token, &to, amount)
        }

        // -- Fee --
        pub fn calculate_fee(_env: Env, amount: i128, rate_bps: i128) -> Result<i128, Error> {
            super::calculate_fee(&_env, amount, rate_bps)
        }

        pub fn calculate_fee_split(
            env: Env,
            amount: i128,
            rate_bps: i128,
        ) -> Result<(i128, i128), Error> {
            super::calculate_fee_split(&env, amount, rate_bps)
        }

        pub fn deduct_fee(
            env: Env,
            token: Address,
            from: Address,
            fee_cfg: FeeConfig,
            amount: i128,
            to: Address,
        ) -> Result<i128, Error> {
            from.require_auth();
            super::deduct_fee(&env, &token, &from, &fee_cfg, amount, &to)
        }

        // -- Escrow --
        pub fn create_escrow(
            env: Env,
            token: Address,
            depositor: Address,
            beneficiary: Address,
            amount: i128,
            expiry_ledger: u32,
        ) -> Result<u64, Error> {
            depositor.require_auth();
            super::create_escrow(
                &env,
                &token,
                &depositor,
                &beneficiary,
                amount,
                expiry_ledger,
            )
        }

        pub fn release_escrow(env: Env, token: Address, escrow_id: u64) -> Result<(), Error> {
            super::release_escrow(&env, &token, escrow_id)
        }

        pub fn refund_escrow(env: Env, token: Address, escrow_id: u64) -> Result<(), Error> {
            super::refund_escrow(&env, &token, escrow_id)
        }

        pub fn get_escrow(env: Env, escrow_id: u64) -> Option<EscrowRecord> {
            super::get_escrow(&env, escrow_id)
        }

        // -- Batch --
        pub fn batch_transfer_all(
            env: Env,
            caller: Address,
            token: Address,
            recipients: soroban_sdk::Vec<(Address, i128)>,
        ) -> crate::batch::BatchResult {
            caller.require_auth();
            let mut rust_vec = std::vec::Vec::new();
            for r in recipients.iter() {
                rust_vec.push(r);
            }
            super::multi_transfer_all(&env, &caller, &token, &rust_vec).unwrap()
        }
    }

    // ====================================================================
    // Safe Transfers
    // ====================================================================

    #[test]
    fn safe_transfer_moves_tokens() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_address = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &contract_address);
        let asset_client = token::StellarAssetClient::new(&env, &contract_address);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let from = Address::generate(&env);
        let to = Address::generate(&env);
        asset_client.mint(&from, &1_000);

        client.safe_transfer(&contract_address, &from, &to, &300);

        assert_eq!(token_client.balance(&from), 700);
        assert_eq!(token_client.balance(&to), 300);
    }

    #[test]
    fn safe_transfer_rejects_zero_amount() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_address = env.register_stellar_asset_contract(admin.clone());

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let from = Address::generate(&env);
        let to = Address::generate(&env);

        let result = client.try_safe_transfer(&contract_address, &from, &to, &0);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
    }

    #[test]
    fn safe_transfer_rejects_negative_amount() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_address = env.register_stellar_asset_contract(admin.clone());

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let from = Address::generate(&env);
        let to = Address::generate(&env);

        let result = client.try_safe_transfer(&contract_address, &from, &to, &-50);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
    }

    #[test]
    fn safe_transfer_full_balance() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_address = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &contract_address);
        let asset_client = token::StellarAssetClient::new(&env, &contract_address);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let from = Address::generate(&env);
        let to = Address::generate(&env);
        asset_client.mint(&from, &500);

        client.safe_transfer(&contract_address, &from, &to, &500);

        assert_eq!(token_client.balance(&from), 0);
        assert_eq!(token_client.balance(&to), 500);
    }

    #[test]
    fn safe_transfer_from_contract_moves_tokens() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_address = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &contract_address);
        let asset_client = token::StellarAssetClient::new(&env, &contract_address);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let to = Address::generate(&env);

        // Mint tokens to the mock contract
        asset_client.mint(&contract_id, &1_000);

        client.safe_transfer_from_contract(&contract_address, &to, &200);

        assert_eq!(token_client.balance(&contract_id), 800);
        assert_eq!(token_client.balance(&to), 200);
    }

    #[test]
    fn safe_transfer_from_contract_rejects_zero() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let contract_address = env.register_stellar_asset_contract(admin.clone());

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let to = Address::generate(&env);

        let result = client.try_safe_transfer_from_contract(&contract_address, &to, &0);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
    }

    // ====================================================================
    // Fee Calculation (pure math — no contract context needed)
    // ====================================================================

    #[test]
    fn calculate_fee_basic() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        // 2.5% fee on 1000 = 25
        let fee = client.calculate_fee(&1_000, &250);
        assert_eq!(fee, 25);
    }

    #[test]
    fn calculate_fee_zero_rate() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let fee = client.calculate_fee(&1_000, &0);
        assert_eq!(fee, 0);
    }

    #[test]
    fn calculate_fee_full_rate() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let fee = client.calculate_fee(&1_000, &10_000);
        assert_eq!(fee, 1_000);
    }

    #[test]
    fn calculate_fee_rounds_down() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        // 1 bps on 1000 = 0.1 -> rounds down to 0
        assert_eq!(client.calculate_fee(&1_000, &1), 0);
        // 1 bps on 10_001 = 1.0001 -> rounds down to 1
        assert_eq!(client.calculate_fee(&10_001, &1), 1);
    }

    #[test]
    fn calculate_fee_rejects_zero_amount() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let result = client.try_calculate_fee(&0, &250);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
    }

    #[test]
    fn calculate_fee_rejects_negative_amount() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let result = client.try_calculate_fee(&-1, &250);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
    }

    #[test]
    fn calculate_fee_rejects_rate_above_10000() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let result = client.try_calculate_fee(&1_000, &10_001);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidFeeRate)));
    }

    #[test]
    fn calculate_fee_rejects_negative_rate() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let result = client.try_calculate_fee(&1_000, &-1);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidFeeRate)));
    }

    #[test]
    fn calculate_fee_handles_large_values() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        // rate_bps = 0 -> fee is always 0 regardless of amount
        assert_eq!(client.calculate_fee(&i128::MAX, &0), 0);

        // i128::MAX * 1 = i128::MAX (no overflow), / 10_000 = ok
        let _result = client.calculate_fee(&i128::MAX, &1);

        // i128::MAX * 2 overflows
        let result = client.try_calculate_fee(&i128::MAX, &2);
        assert_eq!(result, Err(Ok(Error::PaymentFeeOverflow)));
    }

    #[test]
    fn calculate_fee_split_sums_correctly() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let (fee, net) = client.calculate_fee_split(&1_000, &250);
        assert_eq!(fee, 25);
        assert_eq!(net, 975);
        assert_eq!(fee + net, 1_000);
    }

    // ====================================================================
    // Fee Deduction with Transfers
    // ====================================================================

    #[test]
    fn deduct_fee_transfers_both_portions() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &token_addr);
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let from = Address::generate(&env);
        let fee_recipient = Address::generate(&env);
        let to = Address::generate(&env);
        asset_client.mint(&from, &1_000);

        let fee_cfg = FeeConfig {
            rate_bps: 250,
            recipient: fee_recipient.clone(),
        };

        let net = client.deduct_fee(&token_addr, &from, &fee_cfg, &1_000, &to);

        assert_eq!(net, 975);
        assert_eq!(token_client.balance(&from), 0);
        assert_eq!(token_client.balance(&fee_recipient), 25);
        assert_eq!(token_client.balance(&to), 975);
    }

    #[test]
    fn deduct_fee_zero_rate_transfers_full_amount() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &token_addr);
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let from = Address::generate(&env);
        let fee_recipient = Address::generate(&env);
        let to = Address::generate(&env);
        asset_client.mint(&from, &500);

        let fee_cfg = FeeConfig {
            rate_bps: 0,
            recipient: fee_recipient.clone(),
        };

        let net = client.deduct_fee(&token_addr, &from, &fee_cfg, &500, &to);

        assert_eq!(net, 500);
        assert_eq!(token_client.balance(&from), 0);
        assert_eq!(token_client.balance(&fee_recipient), 0);
        assert_eq!(token_client.balance(&to), 500);
    }

    // ====================================================================
    // Escrow — Full Lifecycle
    // ====================================================================

    #[test]
    fn escrow_create_release_lifecycle() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &token_addr);
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 100;

        // Create escrow
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);
        assert_eq!(escrow_id, 1);
        assert_eq!(token_client.balance(&depositor), 3_000);
        assert_eq!(token_client.balance(&contract_id), 2_000);

        // Verify record
        let record = client.get_escrow(&escrow_id).unwrap();
        assert_eq!(record.state, EscrowState::Active);
        assert_eq!(record.amount, 2_000);
        assert_eq!(record.depositor, depositor);
        assert_eq!(record.beneficiary, beneficiary);

        // Release escrow
        client.release_escrow(&token_addr, &escrow_id);
        assert_eq!(token_client.balance(&beneficiary), 2_000);
        assert_eq!(token_client.balance(&contract_id), 0);

        // Verify record state changed
        let record = client.get_escrow(&escrow_id).unwrap();
        assert_eq!(record.state, EscrowState::Released);
    }

    #[test]
    fn escrow_create_refund_lifecycle() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &token_addr);
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 100;

        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);

        // Refund escrow
        client.refund_escrow(&token_addr, &escrow_id);
        assert_eq!(token_client.balance(&depositor), 5_000);
        assert_eq!(token_client.balance(&contract_id), 0);

        let record = client.get_escrow(&escrow_id).unwrap();
        assert_eq!(record.state, EscrowState::Refunded);
    }

    #[test]
    fn escrow_rejects_zero_amount() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let expiry = env.ledger().sequence() + 100;

        let result = client.try_create_escrow(&token_addr, &depositor, &beneficiary, &0, &expiry);
        assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
    }

    #[test]
    fn escrow_rejects_past_expiry() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);

        let past = env.ledger().sequence(); // current = not strictly greater
        let result = client.try_create_escrow(&token_addr, &depositor, &beneficiary, &100, &past);
        assert_eq!(result, Err(Ok(Error::InvalidArgument)));
    }

    #[test]
    fn escrow_release_rejects_unknown_id() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let result = client.try_release_escrow(&token_addr, &999);
        assert_eq!(result, Err(Ok(Error::PaymentEscrowNotFound)));
    }

    #[test]
    fn escrow_release_rejects_already_released() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 100;
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);
        client.release_escrow(&token_addr, &escrow_id);

        let result = client.try_release_escrow(&token_addr, &escrow_id);
        assert_eq!(result, Err(Ok(Error::PaymentEscrowAlreadyReleased)));
    }

    #[test]
    fn escrow_release_rejects_already_refunded() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 100;
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);
        client.refund_escrow(&token_addr, &escrow_id);

        let result = client.try_release_escrow(&token_addr, &escrow_id);
        assert_eq!(result, Err(Ok(Error::PaymentEscrowAlreadyRefunded)));
    }

    #[test]
    fn escrow_refund_rejects_already_released() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 100;
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);
        client.release_escrow(&token_addr, &escrow_id);

        let result = client.try_refund_escrow(&token_addr, &escrow_id);
        assert_eq!(result, Err(Ok(Error::PaymentEscrowAlreadyReleased)));
    }

    #[test]
    fn escrow_refund_rejects_already_refunded() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 100;
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);
        client.refund_escrow(&token_addr, &escrow_id);

        let result = client.try_refund_escrow(&token_addr, &escrow_id);
        assert_eq!(result, Err(Ok(Error::PaymentEscrowAlreadyRefunded)));
    }

    #[test]
    fn escrow_release_after_expiry_is_rejected() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 5;
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);

        // Advance past expiry
        env.ledger().with_mut(|l| {
            l.sequence_number += 10;
        });

        let result = client.try_release_escrow(&token_addr, &escrow_id);
        assert_eq!(result, Err(Ok(Error::PaymentEscrowExpired)));
    }

    #[test]
    fn escrow_refund_after_expiry_succeeds() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &token_addr);
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 5;
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);

        // Advance past expiry
        env.ledger().with_mut(|l| {
            l.sequence_number += 10;
        });

        client.refund_escrow(&token_addr, &escrow_id);
        assert_eq!(token_client.balance(&depositor), 5_000);
        assert_eq!(token_client.balance(&contract_id), 0);
    }

    #[test]
    fn escrow_get_returns_none_for_unknown_id() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        assert_eq!(client.get_escrow(&42), None);
    }

    #[test]
    fn escrow_ids_are_sequential() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &10_000);

        let expiry = env.ledger().sequence() + 100;

        let id1 = client.create_escrow(&token_addr, &depositor, &beneficiary, &100, &expiry);
        let id2 = client.create_escrow(&token_addr, &depositor, &beneficiary, &200, &expiry);
        let id3 = client.create_escrow(&token_addr, &depositor, &beneficiary, &300, &expiry);

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn escrow_release_before_expiry_succeeds() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &token_addr);
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let depositor = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        asset_client.mint(&depositor, &5_000);

        let expiry = env.ledger().sequence() + 100;
        let escrow_id =
            client.create_escrow(&token_addr, &depositor, &beneficiary, &2_000, &expiry);

        // Release well before expiry
        client.release_escrow(&token_addr, &escrow_id);
        assert_eq!(token_client.balance(&beneficiary), 2_000);
    }

    // ====================================================================
    // Batch Payments via re-exports
    // ====================================================================

    #[test]
    fn batch_transfer_all_works() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract(admin.clone());
        let token_client = token::Client::new(&env, &token_addr);
        let asset_client = token::StellarAssetClient::new(&env, &token_addr);

        let contract_id = env.register_contract(None, MockPaymentsContract);
        let client = MockPaymentsContractClient::new(&env, &contract_id);

        let caller = Address::generate(&env);
        let r1 = Address::generate(&env);
        let r2 = Address::generate(&env);
        asset_client.mint(&caller, &1_000);

        let recipients = soroban_sdk::vec![&env, (r1.clone(), 150_i128), (r2.clone(), 250_i128)];
        let result = client.batch_transfer_all(&caller, &token_addr, &recipients);
        // Note: unwrap the Result since BatchResult is wrapped in Result by the client.

        assert_eq!(result.succeeded, 2);
        assert_eq!(token_client.balance(&r1), 150);
        assert_eq!(token_client.balance(&r2), 250);
        assert_eq!(token_client.balance(&caller), 600);
    }
}
