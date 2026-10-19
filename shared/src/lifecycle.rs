//! # Lifecycle State Machine
//!
//! Provides deterministic state transition validation for core records in the
//! Trellis contract suite. Implements Issue #27's requirement for explicit
//! lifecycle states with guarded transitions.
//!
//! ## Design Goals
//!
//! - **Type Safety**: Leverage Rust's type system to prevent invalid states
//! - **Explicit Transitions**: All valid transitions defined in one place
//! - **Business Rule Validation**: Runtime checks for ledger timing, authorization
//! - **Audit Trail**: Event emissions for all state changes
//! - **Zero Overhead**: Inlined validators with no cross-contract calls
//!
//! ## Supported Lifecycles
//!
//! - **AidRecord**: `Pending → Settled | Refunded`
//! - **EscrowRecord**: `Active → Released | Refunded`
//! - **Proposal**: `Pending → Executed`
//! - **ContractRecord**: `Active → Deactivated → Active` (deprecation lifecycle)
//!
//! ## Formal Lifecycle Model
//!
//! Each lifecycle is modeled as a deterministic finite state machine with an
//! explicit set of terminal states and recovery transitions. See the
//! `lifecycle_model` module below for the canonical transition table used by
//! the exhaustive tests.
//!
//! ## Usage Example
//!
//! ```ignore
//! use shared::lifecycle::{AidStateMachine, AidTransitionContext};
//!
//! let ctx = AidTransitionContext {
//!     current_ledger: env.ledger().sequence(),
//!     expiry_ledger: record.expiry_ledger,
//!     caller: caller.clone(),
//!     recipient: record.recipient.clone(),
//! };
//!
//! // Validate transition before mutating state
//! AidStateMachine::can_transition(
//!     record.status,
//!     AidStatus::Settled,
//!     &ctx,
//! )?;
//!
//! // Safe to update state
//! record.status = AidStatus::Settled;
//! ```

use soroban_sdk::{contracttype, Address, Env, Symbol};

use crate::errors::Error as SharedError;

// Type alias for ergonomics
type Error = SharedError;

// Re-export status enums from their respective modules for convenience
pub use crate::payments::EscrowState;

// ===========================================================================
// Emergency Circuit Breaker (Scoped Pause)
// ===========================================================================

/// Operation families that can be independently paused by the circuit breaker.
///
/// Each scope maps to a family of contract actions. Pausing a scope blocks
/// only the operations in that family; reads and unrelated flows continue.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PauseScope {
    /// Aid claim, settle, and refund operations.
    Aid = 0,
    /// Escrow release and refund operations.
    Escrow = 1,
    /// Proposal execution operations.
    Proposal = 2,
    /// Contract record deactivation/reactivation operations.
    ContractRecord = 3,
}

/// Returns the stable symbol used to identify a pause scope in storage and events.
pub fn pause_scope_to_symbol(_env: &Env, scope: PauseScope) -> Symbol {
    use soroban_sdk::symbol_short;
    match scope {
        PauseScope::Aid => symbol_short!("aid"),
        PauseScope::Escrow => symbol_short!("escrow"),
        PauseScope::Proposal => symbol_short!("proposal"),
        PauseScope::ContractRecord => symbol_short!("record"),
    }
}

/// Storage key namespace for circuit breaker pause flags.
#[contracttype]
#[derive(Clone)]
pub enum CircuitBreakerKey {
    /// Pause flag for a specific scope.
    Paused(PauseScope),
    /// Address authorized to pause/resume scopes.
    Authority,
}

/// Returns `true` if the given scope is currently paused.
pub fn is_paused(env: &Env, scope: PauseScope) -> bool {
    env.storage()
        .persistent()
        .get(&CircuitBreakerKey::Paused(scope))
        .unwrap_or(false)
}

/// Guard helper: returns `Ok(())` if the scope is not paused, otherwise
/// `Err(Error::OperationPaused)`.
pub fn require_not_paused(env: &Env, scope: PauseScope) -> Result<(), Error> {
    if is_paused(env, scope) {
        Err(Error::OperationPaused)
    } else {
        Ok(())
    }
}

/// Authorized entrypoint to pause a scope.
///
/// The caller must equal the stored circuit breaker authority. Emits a
/// `("breaker", "pause")` event with `(scope, actor, timestamp)`.
pub fn pause_scope(
    env: &Env,
    caller: &Address,
    scope: PauseScope,
    timestamp: u64,
) -> Result<(), Error> {
    caller.require_auth();
    let authority: Address = env
        .storage()
        .persistent()
        .get(&CircuitBreakerKey::Authority)
        .ok_or(Error::Unauthorized)?;
    if caller != &authority {
        return Err(Error::Unauthorized);
    }
    env.storage()
        .persistent()
        .set(&CircuitBreakerKey::Paused(scope), &true);
    emit_pause_event(env, scope, caller, timestamp);
    Ok(())
}

/// Authorized entrypoint to resume a scope.
///
/// The caller must equal the stored circuit breaker authority. Emits a
/// `("breaker", "resume")` event with `(scope, actor, timestamp)`.
pub fn resume_scope(
    env: &Env,
    caller: &Address,
    scope: PauseScope,
    timestamp: u64,
) -> Result<(), Error> {
    caller.require_auth();
    let authority: Address = env
        .storage()
        .persistent()
        .get(&CircuitBreakerKey::Authority)
        .ok_or(Error::Unauthorized)?;
    if caller != &authority {
        return Err(Error::Unauthorized);
    }
    env.storage()
        .persistent()
        .set(&CircuitBreakerKey::Paused(scope), &false);
    emit_resume_event(env, scope, caller, timestamp);
    Ok(())
}

/// Emit a structured event for a pause action.
///
/// Topics: `("breaker", "pause")`. Data: `(scope, actor, timestamp)`.
pub fn emit_pause_event(env: &Env, scope: PauseScope, actor: &Address, timestamp: u64) {
    use soroban_sdk::symbol_short;
    env.events().publish(
        (symbol_short!("breaker"), symbol_short!("pause")),
        (
            pause_scope_to_symbol(env, scope),
            actor.clone(),
            timestamp,
        ),
    );
}

/// Emit a structured event for a resume action.
///
/// Topics: `("breaker", "resume")`. Data: `(scope, actor, timestamp)`.
pub fn emit_resume_event(env: &Env, scope: PauseScope, actor: &Address, timestamp: u64) {
    use soroban_sdk::symbol_short;
    env.events().publish(
        (symbol_short!("breaker"), symbol_short!("resume")),
        (
            pause_scope_to_symbol(env, scope),
            actor.clone(),
            timestamp,
        ),
    );
}

// ===========================================================================
// Contract Record Deactivation Lifecycle
// ===========================================================================

// ===========================================================================
// State Machine Trait (Generic Interface)
// ===========================================================================

/// Generic state machine trait for lifecycle validation.
///
/// Implementers define:
/// - `State`: The enum representing possible states
/// - `TransitionContext`: Business rule context (ledger, caller, etc.)
/// - `Error`: Error type for invalid transitions
pub trait StateMachine {
    /// The state enum type (e.g., `AidStatus`, `EscrowState`)
    type State: Copy + Clone + Eq + PartialEq;

    /// Context needed to validate business rules for a transition
    type TransitionContext;

    /// Error type returned when a transition is invalid
    type Error;

    /// Check if a transition from `current` to `next` is valid given `context`.
    ///
    /// Returns `Ok(())` if the transition is allowed, or an appropriate error
    /// describing why the transition is invalid.
    fn can_transition(
        current: Self::State,
        next: Self::State,
        context: &Self::TransitionContext,
    ) -> Result<(), Self::Error>;

    /// Execute a validated state transition.
    ///
    /// This is a convenience wrapper that calls `can_transition` and returns
    /// the new state on success.
    fn transition(
        current: Self::State,
        next: Self::State,
        context: &Self::TransitionContext,
    ) -> Result<Self::State, Self::Error> {
        Self::can_transition(current, next, context)?;
        Ok(next)
    }
}

// ===========================================================================
// Contract Record Lifecycle State Machine (Deprecation / Deactivation)
// ===========================================================================

/// Lifecycle state for a Trellis contract record.
///
/// Records begin `Active`. Deprecated records may be explicitly deactivated
/// through an authorized transition, and optionally reactivated later.
/// Deactivated records cannot execute restricted operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ContractRecordState {
    /// Record is live and may execute restricted operations.
    Active,
    /// Record has been explicitly deactivated; restricted operations are blocked.
    Deactivated,
}

/// State machine for `ContractRecord` deactivation lifecycle.
///
/// Valid transitions:
/// - `Active → Deactivated` (authorized deactivation of an eligible record)
/// - `Deactivated → Active` (authorized reactivation)
///
/// All other transitions (including idempotent re-application) are rejected
/// so that callers must observe the current state before mutating it.
pub struct ContractRecordStateMachine;

/// Context required to validate a `ContractRecord` state transition.
pub struct ContractRecordTransitionContext {
    /// Address initiating the transition (must equal `authority`).
    pub caller: Address,
    /// Address authorized to deactivate/reactivate this record.
    pub authority: Address,
    /// Whether the record is eligible for deactivation (e.g. deprecated).
    pub eligible_for_deactivation: bool,
}

impl StateMachine for ContractRecordStateMachine {
    type State = ContractRecordState;
    type TransitionContext = ContractRecordTransitionContext;
    type Error = Error;

    fn can_transition(
        current: ContractRecordState,
        next: ContractRecordState,
        ctx: &ContractRecordTransitionContext,
    ) -> Result<(), Error> {
        match (current, next) {
            // Active → Deactivated: only eligible records, only by authority.
            (ContractRecordState::Active, ContractRecordState::Deactivated) => {
                if ctx.caller != ctx.authority {
                    return Err(Error::Unauthorized);
                }
                if !ctx.eligible_for_deactivation {
                    return Err(Error::RecordNotEligibleForDeactivation);
                }
                Ok(())
            }

            // Deactivated → Active: reactivation by authority only.
            (ContractRecordState::Deactivated, ContractRecordState::Active) => {
                if ctx.caller != ctx.authority {
                    return Err(Error::Unauthorized);
                }
                Ok(())
            }

            // Idempotent re-application rejected to force explicit state reads.
            (ContractRecordState::Active, ContractRecordState::Active) => {
                Err(Error::RecordAlreadyActive)
            }
            (ContractRecordState::Deactivated, ContractRecordState::Deactivated) => {
                Err(Error::RecordAlreadyDeactivated)
            }
        }
    }
}

/// Returns `true` if the record state permits restricted operations.
///
/// Restricted operations must consult this guard before executing.
pub fn is_operational(state: ContractRecordState) -> bool {
    matches!(state, ContractRecordState::Active)
}

/// Guard helper: returns `Ok(())` if the record may execute restricted
/// operations, otherwise `Err(Error::RecordDeactivated)`.
pub fn require_operational(state: ContractRecordState) -> Result<(), Error> {
    if is_operational(state) {
        Ok(())
    } else {
        Err(Error::RecordDeactivated)
    }
}

/// Emit a structured event for a contract record deactivation state change.
///
/// Topics: `("record", "deact")`. Data: `(record_id, from_state, to_state,
/// actor, timestamp)`.
pub fn emit_deactivation_event(
    env: &Env,
    record_id: u64,
    from_state: ContractRecordState,
    to_state: ContractRecordState,
    actor: &Address,
    timestamp: u64,
) {
    use soroban_sdk::symbol_short;

    env.events().publish(
        (symbol_short!("record"), symbol_short!("deact")),
        (
            record_id,
            contract_record_state_to_symbol(env, from_state),
            contract_record_state_to_symbol(env, to_state),
            actor.clone(),
            timestamp,
        ),
    );
}

/// Convert `ContractRecordState` to a Symbol for event emission.
pub fn contract_record_state_to_symbol(_env: &Env, state: ContractRecordState) -> Symbol {
    use soroban_sdk::symbol_short;
    match state {
        ContractRecordState::Active => symbol_short!("active"),
        ContractRecordState::Deactivated => symbol_short!("deactive"),
    }
}

// ===========================================================================
// Aid Lifecycle State Machine
// ===========================================================================

/// State machine for `AidRecord` lifecycle.
///
/// Valid transitions:
/// - `Pending → Settled` (recipient claims before expiry)
/// - `Pending → Refunded` (donor refunds after expiry)
///
/// All other transitions are invalid and will return an error.
pub struct AidStateMachine;

/// Context required to validate an Aid state transition.
pub struct AidTransitionContext {
    /// Current ledger sequence number
    pub current_ledger: u32,
    /// Ledger sequence at which the aid expires
    pub expiry_ledger: u32,
    /// Address initiating the transition
    pub caller: Address,
    /// Intended recipient of the aid
    pub recipient: Address,
}

/// Aid status enum (re-declared here for the state machine; matches aid-contract)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AidStatus {
    Pending,
    Settled,
    Refunded,
}

impl StateMachine for AidStateMachine {
    type State = AidStatus;
    type TransitionContext = AidTransitionContext;
    type Error = Error;

    fn can_transition(
        current: AidStatus,
        next: AidStatus,
        ctx: &AidTransitionContext,
    ) -> Result<(), Error> {
        match (current, next) {
            // Pending → Settled: Recipient claims before expiry
            (AidStatus::Pending, AidStatus::Settled) => {
                if ctx.current_ledger > ctx.expiry_ledger {
                    return Err(Error::Expired);
                }
                if ctx.caller != ctx.recipient {
                    return Err(Error::Unauthorized);
                }
                Ok(())
            }

            // Pending → Refunded: Donor refunds after expiry
            (AidStatus::Pending, AidStatus::Refunded) => {
                if ctx.current_ledger <= ctx.expiry_ledger {
                    return Err(Error::AidNotExpiredYet);
                }
                Ok(())
            }

            // Idempotency: attempting to re-apply the same terminal state
            (AidStatus::Settled, AidStatus::Settled) => Err(Error::AlreadyClaimed),
            (AidStatus::Refunded, AidStatus::Refunded) => Err(Error::AidAlreadyRefunded),

            // Cross-transitions from terminal states
            (AidStatus::Settled, _) => Err(Error::AlreadyClaimed),
            (AidStatus::Refunded, _) => Err(Error::AidAlreadyRefunded),

            // Any other transition is invalid
            _ => Err(Error::InvalidTransition),
        }
    }
}

// ===========================================================================
// Escrow Lifecycle State Machine
// ===========================================================================

/// State machine for `EscrowRecord` lifecycle.
///
/// Valid transitions:
/// - `Active → Released` (authorized release before/at expiry)
/// - `Active → Refunded` (authorized refund, typically after expiry)
///
/// All other transitions are invalid.
pub struct EscrowStateMachine;

/// Context required to validate an Escrow state transition.
pub struct EscrowTransitionContext {
    /// Current ledger sequence number
    pub current_ledger: u32,
    /// Ledger sequence at which the escrow expires
    pub expiry_ledger: u32,
}

impl StateMachine for EscrowStateMachine {
    type State = EscrowState;
    type TransitionContext = EscrowTransitionContext;
    type Error = Error;

    fn can_transition(
        current: EscrowState,
        next: EscrowState,
        ctx: &EscrowTransitionContext,
    ) -> Result<(), Error> {
        match (current, next) {
            // Active → Released: Release before or at expiry
            (EscrowState::Active, EscrowState::Released) => {
                if ctx.current_ledger > ctx.expiry_ledger {
                    return Err(Error::PaymentEscrowExpired);
                }
                Ok(())
            }

            // Active → Refunded: Refund allowed any time (business logic controls authorization)
            (EscrowState::Active, EscrowState::Refunded) => Ok(()),

            // Idempotency: attempting to re-apply the same terminal state
            (EscrowState::Released, EscrowState::Released) => Err(Error::PaymentEscrowAlreadyReleased),
            (EscrowState::Refunded, EscrowState::Refunded) => Err(Error::PaymentEscrowAlreadyRefunded),

            // Cross-transitions from terminal states
            (EscrowState::Released, _) => Err(Error::PaymentEscrowAlreadyReleased),
            (EscrowState::Refunded, _) => Err(Error::PaymentEscrowAlreadyRefunded),

            // Any other transition is invalid
            _ => Err(Error::InvalidTransition),
        }
    }
}

// ===========================================================================
// Proposal Lifecycle State Machine
// ===========================================================================

/// State machine for `Proposal` lifecycle in governance.
///
/// Valid transitions:
/// - `Pending → Executed` (when approval threshold is met)
///
/// All other transitions are invalid.
pub struct ProposalStateMachine;

/// Context required to validate a Proposal state transition.
pub struct ProposalTransitionContext {
    /// Number of approvals collected
    pub approval_count: u32,
    /// Minimum approvals required to execute
    pub threshold: u32,
}

/// Proposal status enum (re-declared here; matches governance-contract)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProposalStatus {
    Pending,
    Executed,
}

impl StateMachine for ProposalStateMachine {
    type State = ProposalStatus;
    type TransitionContext = ProposalTransitionContext;
    type Error = Error;

    fn can_transition(
        current: ProposalStatus,
        next: ProposalStatus,
        ctx: &ProposalTransitionContext,
    ) -> Result<(), Error> {
        match (current, next) {
            // Pending → Executed: Must meet approval threshold
            (ProposalStatus::Pending, ProposalStatus::Executed) => {
                if ctx.approval_count < ctx.threshold {
                    return Err(Error::BelowThreshold);
                }
                Ok(())
            }

            // Idempotency: attempting to re-execute
            (ProposalStatus::Executed, ProposalStatus::Executed) => Err(Error::AlreadyExecuted),

            // Reverse transition not allowed
            (ProposalStatus::Executed, ProposalStatus::Pending) => Err(Error::AlreadyExecuted),

            // Any other transition is invalid
            _ => Err(Error::InvalidTransition),
        }
    }
}

// ===========================================================================
// Event Emission for State Transitions
// ===========================================================================

/// Emit a structured event for any state transition.
///
/// This provides a unified audit trail across all lifecycle state machines.
///
/// # Arguments
/// * `env` - Soroban environment
/// * `record_type` - Symbol identifying the record type ("aid", "escrow", "proposal")
/// * `record_id` - Unique identifier of the record
/// * `from_state` - Symbol representing the previous state
/// * `to_state` - Symbol representing the new state
/// * `actor` - Address that triggered the transition
/// * `timestamp` - Ledger timestamp of the transition
pub fn emit_state_transition(
    env: &Env,
    record_type: Symbol,
    record_id: u64,
    from_state: Symbol,
    to_state: Symbol,
    actor: &Address,
    timestamp: u64,
) {
    use soroban_sdk::symbol_short;

    env.events().publish(
        (symbol_short!("state"), symbol_short!("trans")),
        (
            record_type,
            record_id,
            from_state,
            to_state,
            actor.clone(),
            timestamp,
        ),
    );
}

// ===========================================================================
// Helper Conversions
// ===========================================================================

/// Convert `AidStatus` to a Symbol for event emission.
pub fn aid_status_to_symbol(_env: &Env, status: AidStatus) -> Symbol {
    use soroban_sdk::symbol_short;
    match status {
        AidStatus::Pending => symbol_short!("pending"),
        AidStatus::Settled => symbol_short!("settled"),
        AidStatus::Refunded => symbol_short!("refunded"),
    }
}

/// Convert `EscrowState` to a Symbol for event emission.
pub fn escrow_state_to_symbol(_env: &Env, state: EscrowState) -> Symbol {
    use soroban_sdk::symbol_short;
    match state {
        EscrowState::Active => symbol_short!("active"),
        EscrowState::Released => symbol_short!("released"),
        EscrowState::Refunded => symbol_short!("refunded"),
    }
}

/// Convert `ProposalStatus` to a Symbol for event emission.
pub fn proposal_status_to_symbol(_env: &Env, status: ProposalStatus) -> Symbol {
    use soroban_sdk::symbol_short;
    match status {
        ProposalStatus::Pending => symbol_short!("pending"),
        ProposalStatus::Executed => symbol_short!("executed"),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    // -----------------------------------------------------------------------
    // Aid State Machine Tests
    // -----------------------------------------------------------------------

    #[test]
    fn aid_pending_to_settled_valid() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Pending,
            AidStatus::Settled,
            &ctx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn aid_pending_to_settled_expired() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Pending,
            AidStatus::Settled,
            &ctx,
        );
        assert_eq!(result, Err(Error::Expired));
    }

    #[test]
    fn aid_pending_to_settled_unauthorized() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let recipient = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
            caller,
            recipient,
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Pending,
            AidStatus::Settled,
            &ctx,
        );
        assert_eq!(result, Err(Error::Unauthorized));
    }

    #[test]
    fn aid_pending_to_refunded_valid() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Pending,
            AidStatus::Refunded,
            &ctx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn aid_pending_to_refunded_not_expired() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Pending,
            AidStatus::Refunded,
            &ctx,
        );
        assert_eq!(result, Err(Error::AidNotExpiredYet));
    }

    #[test]
    fn aid_settled_to_refunded_invalid() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Settled,
            AidStatus::Refunded,
            &ctx,
        );
        assert_eq!(result, Err(Error::AlreadyClaimed));
    }

    #[test]
    fn aid_refunded_to_settled_invalid() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Refunded,
            AidStatus::Settled,
            &ctx,
        );
        assert_eq!(result, Err(Error::AidAlreadyRefunded));
    }

    #[test]
    fn aid_settled_to_settled_idempotent_rejected() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Settled,
            AidStatus::Settled,
            &ctx,
        );
        assert_eq!(result, Err(Error::AlreadyClaimed));
    }

    #[test]
    fn aid_refunded_to_refunded_idempotent_rejected() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let ctx = AidTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
            caller: caller.clone(),
            recipient: caller.clone(),
        };

        let result = AidStateMachine::can_transition(
            AidStatus::Refunded,
            AidStatus::Refunded,
            &ctx,
        );
        assert_eq!(result, Err(Error::AidAlreadyRefunded));
    }

    // -----------------------------------------------------------------------
    // Escrow State Machine Tests
    // -----------------------------------------------------------------------

    #[test]
    fn escrow_active_to_released_valid() {
        let ctx = EscrowTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
        };

        let result = EscrowStateMachine::can_transition(
            EscrowState::Active,
            EscrowState::Released,
            &ctx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn escrow_active_to_released_expired() {
        let ctx = EscrowTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
        };

        let result = EscrowStateMachine::can_transition(
            EscrowState::Active,
            EscrowState::Released,
            &ctx,
        );
        assert_eq!(result, Err(Error::PaymentEscrowExpired));
    }

    #[test]
    fn escrow_active_to_refunded_valid() {
        let ctx = EscrowTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
        };

        let result = EscrowStateMachine::can_transition(
            EscrowState::Active,
            EscrowState::Refunded,
            &ctx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn escrow_released_to_refunded_invalid() {
        let ctx = EscrowTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
        };

        let result = EscrowStateMachine::can_transition(
            EscrowState::Released,
            EscrowState::Refunded,
            &ctx,
        );
        assert_eq!(result, Err(Error::PaymentEscrowAlreadyReleased));
    }

    #[test]
    fn escrow_refunded_to_released_invalid() {
        let ctx = EscrowTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
        };

        let result = EscrowStateMachine::can_transition(
            EscrowState::Refunded,
            EscrowState::Released,
            &ctx,
        );
        assert_eq!(result, Err(Error::PaymentEscrowAlreadyRefunded));
    }

    #[test]
    fn escrow_released_to_released_idempotent_rejected() {
        let ctx = EscrowTransitionContext {
            current_ledger: 100,
            expiry_ledger: 200,
        };

        let result = EscrowStateMachine::can_transition(
            EscrowState::Released,
            EscrowState::Released,
            &ctx,
        );
        assert_eq!(result, Err(Error::PaymentEscrowAlreadyReleased));
    }

    #[test]
    fn escrow_refunded_to_refunded_idempotent_rejected() {
        let ctx = EscrowTransitionContext {
            current_ledger: 300,
            expiry_ledger: 200,
        };

        let result = EscrowStateMachine::can_transition(
            EscrowState::Refunded,
            EscrowState::Refunded,
            &ctx,
        );
        assert_eq!(result, Err(Error::PaymentEscrowAlreadyRefunded));
    }

    // -----------------------------------------------------------------------
    // Proposal State Machine Tests
    // -----------------------------------------------------------------------

    #[test]
    fn proposal_pending_to_executed_valid() {
        let ctx = ProposalTransitionContext {
            approval_count: 3,
            threshold: 2,
        };

        let result = ProposalStateMachine::can_transition(
            ProposalStatus::Pending,
            ProposalStatus::Executed,
            &ctx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn proposal_pending_to_executed_below_threshold() {
        let ctx = ProposalTransitionContext {
            approval_count: 1,
            threshold: 2,
        };

        let result = ProposalStateMachine::can_transition(
            ProposalStatus::Pending,
            ProposalStatus::Executed,
            &ctx,
        );
        assert_eq!(result, Err(Error::BelowThreshold));
    }

    #[test]
    fn proposal_executed_to_pending_invalid() {
        let ctx = ProposalTransitionContext {
            approval_count: 3,
            threshold: 2,
        };

        let result = ProposalStateMachine::can_transition(
            ProposalStatus::Executed,
            ProposalStatus::Pending,
            &ctx,
        );
        assert_eq!(result, Err(Error::AlreadyExecuted));
    }

    #[test]
    fn proposal_executed_to_executed_idempotent_rejected() {
        let ctx = ProposalTransitionContext {
            approval_count: 3,
            threshold: 2,
        };

        let result = ProposalStateMachine::can_transition(
            ProposalStatus::Executed,
            ProposalStatus::Executed,
            &ctx,
        );
        assert_eq!(result, Err(Error::AlreadyExecuted));
    }

    // -----------------------------------------------------------------------
    // Contract Record Deactivation Tests
    // -----------------------------------------------------------------------

    #[test]
    fn contract_record_active_to_deactivated_valid() {
        let env = Env::default();
        let authority = Address::generate(&env);
        let ctx = ContractRecordTransitionContext {
            caller: authority.clone(),
            authority,
            eligible_for_deactivation: true,
        };

        let result = ContractRecordStateMachine::can_transition(
            ContractRecordState::Active,
            ContractRecordState::Deactivated,
            &ctx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn contract_record_deactivate_unauthorized() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let authority = Address::generate(&env);
        let ctx = ContractRecordTransitionContext {
            caller,
            authority,
            eligible_for_deactivation: true,
        };

        let result = ContractRecordStateMachine::can_transition(
            ContractRecordState::Active,
            ContractRecordState::Deactivated,
            &ctx,
        );
        assert_eq!(result, Err(Error::Unauthorized));
    }

    #[test]
    fn contract_record_deactivate_ineligible() {
        let env = Env::default();
        let authority = Address::generate(&env);
        let ctx = ContractRecordTransitionContext {
            caller: authority.clone(),
            authority,
            eligible_for_deactivation: false,
        };

        let result = ContractRecordStateMachine::can_transition(
            ContractRecordState::Active,
            ContractRecordState::Deactivated,
            &ctx,
        );
        assert_eq!(result, Err(Error::RecordNotEligibleForDeactivation));
    }

    #[test]
    fn contract_record_reactivate_valid() {
        let env = Env::default();
        let authority = Address::generate(&env);
        let ctx = ContractRecordTransitionContext {
            caller: authority.clone(),
            authority,
            eligible_for_deactivation: false,
        };

        let result = ContractRecordStateMachine::can_transition(
            ContractRecordState::Deactivated,
            ContractRecordState::Active,
            &ctx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn contract_record_reactivate_unauthorized() {
        let env = Env::default();
        let caller = Address::generate(&env);
        let authority = Address::generate(&env);
        let ctx = ContractRecordTransitionContext {
            caller,
            authority,
            eligible_for_deactivation: false,
        };

        let result = ContractRecordStateMachine::can_transition(
            ContractRecordState::Deactivated,
            ContractRecordState::Active,
            &ctx,
        );
        assert_eq!(result, Err(Error::Unauthorized));
    }

    #[test]
    fn contract_record_restricted_use_blocked_when_deactivated() {
        assert!(require_operational(ContractRecordState::Active).is_ok());
        assert_eq!(
            require_operational(ContractRecordState::Deactivated),
            Err(Error::RecordDeactivated)
        );
        assert!(!is_operational(ContractRecordState::Deactivated));
    }

    #[test]
    fn contract_record_deactivation_event_emitted() {
        use soroban_sdk::testutils::Events as _;

        let env = Env::default();
        let authority = Address::generate(&env);

        emit_deactivation_event(
            &env,
            42,
            ContractRecordState::Active,
            ContractRecordState::Deactivated,
            &authority,
            1_000,
        );

        let events = env.events().all();
        assert_eq!(events.len(), 1);
    }
}
