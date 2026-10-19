use soroban_sdk::{symbol_short, Address, Env, Symbol};

/// Maximum length of a correlation ID in bytes.
/// Correlation IDs are used to link events emitted as part of the same
/// multi-step workflow (submitter registration, price submission, etc.).
pub const MAX_CORRELATION_ID_LEN: u32 = 64;

/// Error code returned when a correlation ID is invalid.
pub const ERR_INVALID_CORRELATION_ID: u32 = 100;

/// Validate a correlation ID according to policy:
/// - Must be non-empty.
/// - Must be at most MAX_CORRELATION_ID_LEN bytes.
/// - Must only contain ASCII alphanumeric characters, hyphens, or underscales.
/// Returns the normalized (trimmed) ID on success, or Err(ERR_INVALID_CORRELATION_ID).
pub fn validate_correlation_id(id: &Symbol) -> Result<Symbol, Symbol> {
    // Symbols in Soroban are limited to 32 bytes, so any valid ID is auto-
/// bounded. We enforce non-empty and character policy by converting to
/// a byte representation and checking each byte.
    let len = id.len();
    if len == 0 || len > MAX_CORRELATION_ID_LEN {
        return Err(ERR_INVALID_CORRELATION_ID);
    }
    // Symbol is always valid ASCII alphanumeric/underscore in Soroban, so the
    // character policy is enforced at the type level. We still perform an
    // explicit check to guard against future changes and to document the policy.
    let bytes = id.to_bytes();
    for b in bytes.iter() {
        let is_alphanum = (b >= b'a' && b <= b'z')
            || (b >= b'A' && b <= b'Z')
            || (b >= b'0' && b <= b'9');
        let is_separator = b == b'-' || b == b'_';
        if !is_alphanum && !is_separator {
            return Err(ERR_INVALID_CORRELATION_ID);
        }
    }
    Ok_id(id.clone())
}

/// Helper to normalize a correlation ID: validate and return it.
/// Panics with ERR_INVALID_CORRELATION_ID if invalid. Used internally by
/// event emitters that are called after the caller has already validated.
pub fn normalize_correlation_id(id: &Symbol) -> Symbol {
    match validate_correlation_id(id) {
        Ok(normalized) => normalized,
        Err(_) => panic_with_error(env_placeholder(), ERR_INVALID_CORRELATION_ID),
    }
}

/// Placeholder for panic context; the actual env is not needed for the
/// error code since Soroban panics are handled by the runtime.
fn env_placeholder() -> &Env {
    unreachable()
}

/// Emit when a submitter is registered.
/// The correlation ID links this event to other events in the same
/// multi-step workflow (e.g. registration -> activation -> price submission).
pub fn emit_submitter_registered(
    env: &Env,
    submitter: &Address,
    timestamp: u64,
    correlation_id: &Symbol,
) {
    let id = normalize_correlation_id(correlation_id);
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("sub_reg")),
        (submitter.clone(), timestamp, id),
    );
}

/// Emit when a submitter is deactivated.
/// The correlation ID links this event to the original registration and
/// any subsequent operations in the workflow.
pub fn emit_submitter_deactivated(
    env: &Env,
    submitter: &Address,
    timestamp: u64,
    correlation_id: &Symbol,
) {
    let id = normalize_correlation_id(correlation_id);
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("sub_del")),
        (submitter.clone(), timestamp, id),
    );
}

/// Emit when a price is submitted.
/// The correlation ID links this event to the submitter's registration and
/// any other price submissions in the same workflow.
pub fn emit_price_submitted(
    env: &Env,
    feed_id: &Symbol,
    price: i128,
    submitter: &Address,
    timestamp: u64,
    correlation_id: &Symbol,
) {
    let id = normalize_correlation_id(correlation_id);
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("price")),
        (feed_id.clone(), price, submitter.clone(), timestamp, id),
    );
}

/// Emit when a staleness window is updated.
/// The correlation ID links this event to the admin workflow that triggered it.
pub fn emit_staleness_window_set(
    env: &Env,
    window_seconds: u64,
    timestamp: u64,
    correlation_id: &Symbol,
) {
    let id = normalize_correlation_id(correlation_id);
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("stale")),
        (window_seconds, timestamp, id),
    );
}

/// Emit when a submitter stakes collateral.
pub fn emit_submitter_staked(
    env: &Env,
    submitter: &Address,
    amount: i128,
    total_stake: i128,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("staked")),
        (submitter.clone(), amount, total_stake, timestamp),
    );
}

/// Emit when a submitter is slashed.
pub fn emit_submitter_slashed(
    env: &Env,
    submitter: &Address,
    penalty_amount: i128,
    remaining_stake: i128,
    reason: Symbol,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("slashed")),
        (submitter.clone(), penalty_amount, remaining_stake, reason, timestamp),
    );
}

/// Emit when an unbonding request is initiated.
pub fn emit_unbonding_requested(
    env: &Env,
    submitter: &Address,
    amount: i128,
    unlock_time: u64,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("unbond")),
        (submitter.clone(), amount, unlock_time, timestamp),
    );
}

/// Emit when unbonded stake is withdrawn.
pub fn emit_stake_withdrawn(
    env: &Env,
    submitter: &Address,
    amount: i128,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("oracle"), symbol_short!("withdrawn")),
        (submitter.clone(), amount, timestamp),
    );
}

