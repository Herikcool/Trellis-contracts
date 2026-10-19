use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Symbol};

// Legacy single-topic constants retained for backward compatibility.
pub const AID_CREATED: Symbol = symbol_short!("aid_crt");
pub const AID_CLAIMED: Symbol = symbol_short!("aid_clm");
pub const AID_SETTLED: Symbol = symbol_short!("aid_stl");
pub const AID_REFUNDED: Symbol = symbol_short!("aid_ref");
pub const COMMISSION_PAID: Symbol = symbol_short!("com_paid");
pub const REFERRAL_ACCRUED: Symbol = symbol_short!("ref_acc");
pub const REFERRER_SET: Symbol = symbol_short!("ref_set");
pub const TIER_CONFIG_SET: Symbol = symbol_short!("tier_cfg");
pub const TREASURY_SET: Symbol = symbol_short!("trs_set");
pub const TREASURY_DEPOSIT: Symbol = symbol_short!("t_dep");
pub const TREASURY_WITHDRAW: Symbol = symbol_short!("t_wdw");
pub const TREASURY_EMERGENCY_WITHDRAW: Symbol = symbol_short!("t_emrg");
pub const PARAMETER_CHANGED: Symbol = symbol_short!("param_chg");
pub const CONTRACT_PAUSED: Symbol = symbol_short!("paused");
pub const CONTRACT_RESUMED: Symbol = symbol_short!("resumed");
pub const CONTRACT_UPGRADED: Symbol = symbol_short!("upgraded");
pub const CONTRACT_SCOPED_PAUSED: Symbol = symbol_short!("sc_pause");
pub const CONTRACT_SCOPED_RESUMED: Symbol = symbol_short!("sc_resum");
pub const PAUSE_SCOPE_SET: Symbol = symbol_short!("sc_set");
pub const REFERRAL_REGISTERED: Symbol = symbol_short!("ref_reg");
pub const PROPOSAL_CREATED: Symbol = symbol_short!("prop_new");
pub const PROPOSAL_APPROVED: Symbol = symbol_short!("prop_apr");
pub const PROPOSAL_EXECUTED: Symbol = symbol_short!("prop_exc");
pub const PROPOSAL_CANCELLED: Symbol = symbol_short!("prop_can");
pub const PROPOSAL_EXPIRED: Symbol = symbol_short!("prop_exp");
pub const ROLE_GRANTED: Symbol = symbol_short!("role_grt");
pub const ROLE_REVOKED: Symbol = symbol_short!("role_rvk");

// Payment event topic constants.
pub const PAYMENT_TRANSFER: Symbol = symbol_short!("pay_xfr");
pub const PAYMENT_FEE: Symbol = symbol_short!("pay_fee");
pub const PAYMENT_ESCROW_CREATED: Symbol = symbol_short!("pay_esc_c");
pub const PAYMENT_ESCROW_RELEASED: Symbol = symbol_short!("pay_esc_r");
pub const PAYMENT_ESCROW_REFUNDED: Symbol = symbol_short!("pay_esc_f");

// Import event topic constants.
pub const IMPORT_SIMULATED: Symbol = symbol_short!("imp_sim");
pub const IMPORT_COMMITTED: Symbol = symbol_short!("imp_cmt");
pub const IMPORT_FAILED: Symbol = symbol_short!("imp_fail");

// Canonical event-logging topic constants.
pub const EVENT_LOG_INITIALIZED: Symbol = symbol_short!("evt_init");
pub const EVENT_LOG_ACTION: Symbol = symbol_short!("evt_act");
pub const EVENT_LOG_PERMISSION: Symbol = symbol_short!("evt_perm");

// ---------------------------------------------------------------------------
// Correlation ID support
// ---------------------------------------------------------------------------

/// Maximum length (in bytes) of a correlation ID.
pub const CORRELATION_ID_MAX_LEN: u32 = 64;

/// Minimum length (in bytes) of a correlation ID.
pub const CORRELATION_ID_MIN_LEN: u32 = 8;

/// Errors returned when validating a correlation ID.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorrelationIdError {
    /// The ID was empty or shorter than `CORRELATION_ID_MIN_LEN`.
    TooShort = 1,
    /// The ID exceeded `CORRELATION_ID_MAX_LEN` bytes.
    TooLong = 2,
    /// The ID contained a byte outside the allowed alphabet.
    InvalidCharacter = 3,
}

/// Returns `true` if `byte` is permitted in a correlation ID.
///
/// Allowed alphabet: `0-9`, `A-Z`, `a-z`, `-`, `_`, `.`, `:`.
#[inline]
fn is_valid_correlation_byte(byte: u8) -> bool {
    matches!(byte,
        b'0'..=b'9'
        | b'A'..=b'Z'
        | b'a'..=b'z'
        | b'-'
        | b'_'
        | b'.'
        | b':'
    )
}

/// Validates a correlation ID according to the canonical policy.
///
/// Policy:
/// - Length must be within `[CORRELATION_ID_MIN_LEN, CORRELATION_ID_MAX_LEN]`.
/// - Characters must be drawn from `[0-9A-Za-z._:-]`.
///
/// Returns `Ok(())` when valid, otherwise a `CorrelationIdError`.
pub fn validate_correlation_id(id: &BytesN<32>) -> Result<(), CorrelationIdError> {
    let bytes = id.to_array();
    let mut len: u32 = 0;
    for b in bytes.iter() {
        if *b == 0 {
            break;
        }
        len += 1;
    }
    if len < CORRELATION_ID_MIN_LEN {
        return Err(CorrelationIdError::TooShort);
    }
    if len > CORRELATION_ID_MAX_LEN {
        return Err(CorrelationIdError::TooLong);
    }
    for i in 0..len {
        if !is_valid_correlation_byte(bytes[i as usize]) {
            return Err(CorrelationIdError::InvalidCharacter);
        }
    }
    Ok(())
}

/// Normalizes a correlation ID by rejecting invalid input.
///
/// The current policy is strict rejection: invalid IDs are surfaced as
/// `CorrelationIdError` so callers can abort the workflow before emitting
/// any correlated events.
pub fn normalize_correlation_id(
    id: &BytesN<32>,
) -> Result<BytesN<32>, CorrelationIdError> {
    validate_correlation_id(id)?;
    Ok(id.clone())
}

/// Emits `AidCreated`.
///
/// Topics: `("aid", "created")`
///
/// Data:
/// `(aid_id, donor, recipient, amount, created_at, expires_at)`
pub fn emit_aid_created(
    env: &Env,
    correlation_id: &BytesN<32>,
    aid_id: u64,
    donor: &Address,
    recipient: &Address,
    amount: i128,
    created_at: u64,
    expires_at: u64,
) {
    env.events().publish(
        (symbol_short!("aid"), symbol_short!("created"), correlation_id.clone()),
        (
            aid_id,
            donor.clone(),
            recipient.clone(),
            amount,
            created_at,
            expires_at,
        ),
    );
}

/// Emits `AidClaimed`.
///
/// Topics: `("aid", "claimed")`
///
/// Data: `(aid_id, claimant, claimed_at)`
pub fn emit_aid_claimed(
    env: &Env,
    correlation_id: &BytesN<32>,
    aid_id: u64,
    claimant: &Address,
    claimed_at: u64,
) {
    env.events().publish(
        (symbol_short!("aid"), symbol_short!("claimed"), correlation_id.clone()),
        (aid_id, claimant.clone(), claimed_at),
    );
}

/// Emits `AidSettled`.
///
/// Topics: `("aid", "settled")`
///
/// Data: `(aid_id, recipient, amount, settled_at)`
pub fn emit_aid_settled(
    env: &Env,
    correlation_id: &BytesN<32>,
    aid_id: u64,
    recipient: &Address,
    amount: i128,
    settled_at: u64,
) {
    env.events().publish(
        (symbol_short!("aid"), symbol_short!("settled"), correlation_id.clone()),
        (aid_id, recipient.clone(), amount, settled_at),
    );
}

/// Emits `AidRefunded`.
///
/// Topics: `("aid", "refunded")`
///
/// Data: `(aid_id, donor, amount, refunded_at)`
pub fn emit_aid_refunded(
    env: &Env,
    correlation_id: &BytesN<32>,
    aid_id: u64,
    donor: &Address,
    amount: i128,
    refunded_at: u64,
) {
    env.events().publish(
        (symbol_short!("aid"), symbol_short!("refunded"), correlation_id.clone()),
        (aid_id, donor.clone(), amount, refunded_at),
    );
}

/// Emits `CommissionPaid`.
///
/// Topics: `("comm", "paid")`
///
/// Data: `(recipient, amount, paid_at)`
pub fn emit_commission_paid(
    env: &Env,
    correlation_id: &BytesN<32>,
    recipient: &Address,
    token: &Address,
    amount: i128,
    paid_at: u64,
) {
    env.events().publish(
        (symbol_short!("comm"), symbol_short!("paid"), correlation_id.clone()),
        (recipient.clone(), token.clone(), amount, paid_at),
    );
}

/// Emits `TreasuryDeposit`.
///
/// Topics: `("treasury", "deposit")`
///
/// Data: `(category, depositor, token, amount, new_balance)`
pub fn emit_treasury_deposit(
    env: &Env,
    correlation_id: &BytesN<32>,
    category: Symbol,
    depositor: &Address,
    token: &Address,
    amount: i128,
    new_balance: i128,
) {
    env.events().publish(
        (symbol_short!("treasury"), symbol_short!("deposit"), correlation_id.clone()),
        (category, depositor.clone(), token.clone(), amount, new_balance),
    );
}

/// Emits `TreasuryWithdrawal`.
///
/// Topics: `("treasury", "withdraw")`
///
/// Data: `(category, recipient, token, amount, remaining_balance)`
pub fn emit_treasury_withdrawal(
    env: &Env,
    correlation_id: &BytesN<32>,
    category: Symbol,
    recipient: &Address,
    token: &Address,
    amount: i128,
    remaining_balance: i128,
) {
    env.events().publish(
        (symbol_short!("treasury"), symbol_short!("withdraw"), correlation_id.clone()),
        (category, recipient.clone(), token.clone(), amount, remaining_balance),
    );
}

/// Emits `ContractPaused`.
///
/// Topics: `("contract", "paused")`
///
/// Data: `(actor, paused_at)`
pub fn emit_contract_paused(
    env: &Env,
    correlation_id: &BytesN<32>,
    actor: &Address,
    paused_at: u64,
) {
    env.events().publish(
        (symbol_short!("contract"), symbol_short!("paused"), correlation_id.clone()),
        (actor.clone(), paused_at),
    );
}

/// Emits `ContractResumed`.
///
/// Topics: `("contract", "resumed")`
///
/// Data: `(actor, resumed_at)`
pub fn emit_contract_resumed(
    env: &Env,
    correlation_id: &BytesN<32>,
    actor: &Address,
    resumed_at: u64,
) {
    env.events().publish(
        (symbol_short!("contract"), symbol_short!("resumed"), correlation_id.clone()),
        (actor.clone(), resumed_at),
    );
}

/// Emits `ContractScopedPaused`.
///
/// Topics: `("contract", "sc_pause")`
///
/// Data: `(actor, scope, paused_at)`
pub fn emit_contract_scoped_paused(
    env: &Env,
    correlation_id: &BytesN<32>,
    actor: &Address,
    scope: Symbol,
    paused_at: u64,
) {
    env.events().publish(
        (symbol_short!("contract"), symbol_short!("sc_pause"), correlation_id.clone()),
        (actor.clone(), scope, paused_at),
    );
}

/// Emits `ContractScopedResumed`.
///
/// Topics: `("contract", "sc_resum")`
///
/// Data: `(actor, scope, resumed_at)`
pub fn emit_contract_scoped_resumed(
    env: &Env,
    correlation_id: &BytesN<32>,
    actor: &Address,
    scope: Symbol,
    resumed_at: u64,
) {
    env.events().publish(
        (symbol_short!("contract"), symbol_short!("sc_resum"), correlation_id.clone()),
        (actor.clone(), scope, resumed_at),
    );
}

/// Emits `ContractUpgraded`.
///
/// Topics: `("contract", "upgraded")`
///
/// Data: `(actor, wasm_hash, upgraded_at)`
pub fn emit_contract_upgraded(
    env: &Env,
    correlation_id: &BytesN<32>,
    actor: &Address,
    wasm_hash: &BytesN<32>,
    upgraded_at: u64,
) {
    env.events().publish(
        (symbol_short!("contract"), symbol_short!("upgraded"), correlation_id.clone()),
        (actor.clone(), wasm_hash.clone(), upgraded_at),
    );
}

// ---------------------------------------------------------------------------
// Canonical Event Logging helpers
// ---------------------------------------------------------------------------

/// Payload for `ModuleInitialized`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleInitializedEvent {
    pub module: Symbol,
    pub version: u32,
    pub caller: Address,
    pub initialized_at: u64,
}

/// Emits `ModuleInitialized`.
///
/// Topics: `("logging", "init")`
///
/// Data: `(Symbol module, u32 version, Address caller, u64 initialized_at)`
pub fn emit_module_initialized(
    env: &Env,
    correlation_id: &BytesN<32>,
    module: Symbol,
    version: u32,
    caller: &Address,
    initialized_at: u64,
) {
    env.events().publish(
        (symbol_short!("logging"), symbol_short!("init"), correlation_id.clone()),
        (module, version, caller.clone(), initialized_at),
    );
}

/// Payload for `ActionExecuted`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionExecutedEvent {
    pub module: Symbol,
    pub action: Symbol,
    pub caller: Address,
    pub success: bool,
    pub executed_at: u64,
}

/// Emits `ActionExecuted`.
///
/// Topics: `("logging", "action")`
///
/// Data: `(Symbol module, Symbol action, Address caller, bool success, u64 executed_at)`
pub fn emit_action_executed(
    env: &Env,
    correlation_id: &BytesN<32>,
    module: Symbol,
    action: Symbol,
    caller: &Address,
    success: bool,
    executed_at: u64,
) {
    env.events().publish(
        (symbol_short!("logging"), symbol_short!("action"), correlation_id.clone()),
        (module, action, caller.clone(), success, executed_at),
    );
}

/// Payload for `PermissionChanged`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionChangedEvent {
    pub module: Symbol,
    pub role: Symbol,
    pub subject: Address,
    pub granted: bool,
    pub changed_at: u64,
}

/// Emits `PermissionChanged`.
///
/// Topics: `("logging", "perm")`
///
/// Data: `(Symbol module, Symbol role, Address subject, bool granted, u64 changed_at)`
pub fn emit_permission_changed(
    env: &Env,
    correlation_id: &BytesN<32>,
    module: Symbol,
    role: Symbol,
    subject: &Address,
    granted: bool,
    changed_at: u64,
) {
    env.events().publish(
        (symbol_short!("logging"), symbol_short!("perm"), correlation_id.clone()),
        (module, role, subject.clone(), granted, changed_at),
    );
}

/// Emits an event using a legacy single-symbol topic.
///
/// New protocol events should use one of the typed helpers above.
pub fn emit<T: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(env: &Env, topic: Symbol, data: T) {
    env.events().publish((topic,), data);
}

// ---------------------------------------------------------------------------
// Upgradeability event helpers
// ---------------------------------------------------------------------------

/// Topics: ("upgrade", "registered")
pub fn emit_contract_registered(
    env: &Env,
    correlation_id: &BytesN<32>,
    contract_id: &Address,
    name: Symbol,
    version: u32,
    wasm_hash: &BytesN<32>,
    registered_at: u64,
) {
    env.events().publish(
        (symbol_short!("upgrade"), symbol_short!("upg_reg"), correlation_id.clone()),
        (
            contract_id.clone(),
            name,
            version,
            wasm_hash.clone(),
            registered_at,
        ),
    );
}

/// Topics: ("upgrade", "proposed")
pub fn emit_upgrade_proposed(
    env: &Env,
    correlation_id: &BytesN<32>,
    proposal_id: u64,
    contract_id: &Address,
    new_version: u32,
    proposer: &Address,
    proposed_at: u64,
) {
    env.events().publish(
        (symbol_short!("upgrade"), symbol_short!("proposed"), correlation_id.clone()),
        (
            proposal_id,
            contract_id.clone(),
            new_version,
            proposer.clone(),
            proposed_at,
        ),
    );
}

/// Topics: ("upgrade", "executed")
pub fn emit_upgrade_executed(
    env: &Env,
    correlation_id: &BytesN<32>,
    proposal_id: u64,
    contract_id: &Address,
    old_version: u32,
    new_version: u32,
    executor: &Address,
    executed_at: u64,
) {
    env.events().publish(
        (symbol_short!("upgrade"), symbol_short!("executed"), correlation_id.clone()),
        (
            proposal_id,
            contract_id.clone(),
            old_version,
            new_version,
            executor.clone(),
            executed_at,
        ),
    );
}

/// Topics: ("upgrade", "hook_set")
pub fn emit_migration_hook_set(
    env: &Env,
    correlation_id: &BytesN<32>,
    contract_id: &Address,
    hook_addr: &Address,
    set_at: u64,
) {
    env.events().publish(
        (symbol_short!("upgrade"), symbol_short!("hook_set"), correlation_id.clone()),
        (contract_id.clone(), hook_addr.clone(), set_at),
    );
}

/// Topics: ("upgrade", "rolledback")
pub fn emit_upgrade_rolled_back(
    env: &Env,
    correlation_id: &BytesN<32>,
    contract_id: &Address,
    from_version: u32,
    to_version: u32,
    executor: &Address,
    rolled_back_at: u64,
) {
    env.events().publish(
        (symbol_short!("upgrade"), symbol_short!("rollback"), correlation_id.clone()),
        (
            contract_id.clone(),
            from_version,
            to_version,
            executor.clone(),
            rolled_back_at,
        ),
    );
}

/// Emits `ReleaseReadinessEvaluated`.
///
/// Topics: `("readiness", "evaluated", correlation_id)`
///
/// Data: `(release_version, is_ready, passed, failed, missing_gates, timestamp)`
pub fn emit_release_readiness_evaluated(
    env: &Env,
    correlation_id: &BytesN<32>,
    release_version: Symbol,
    is_ready: bool,
    passed: u32,
    failed: u32,
    missing_gates: u32,
    timestamp: u64,
) {
    env.events().publish(
        (
            symbol_short!("readiness"),
            symbol_short!("evaluated"),
            correlation_id.clone(),
        ),
        (release_version, is_ready, passed, failed, missing_gates, timestamp),
    );
}

/// Emits `RollbackRehearsalExecuted`.
///
/// Topics: `("rollback", "rehearse", correlation_id)`
///
/// Data: `(rehearsal_id, module_id, from_version, restored_version, passed, timestamp)`
pub fn emit_rollback_rehearsal_executed(
    env: &Env,
    correlation_id: &BytesN<32>,
    rehearsal_id: u64,
    module_id: u32,
    from_version: u32,
    restored_version: u32,
    passed: bool,
    timestamp: u64,
) {
    env.events().publish(
        (
            symbol_short!("rollback"),
            symbol_short!("rehearse"),
            correlation_id.clone(),
        ),
        (
            rehearsal_id,
            module_id,
            from_version,
            restored_version,
            passed,
            timestamp,
        ),
    );
}

/// Emits `RoleGranted`.
///
/// Topics: `("role", "granted")`
///
/// Data: `(admin, grantee, role_name, timestamp)`
pub fn emit_role_granted(
    env: &Env,
    correlation_id: &BytesN<32>,
    admin: &Address,
    grantee: &Address,
    role_name: Symbol,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("role"), symbol_short!("granted"), correlation_id.clone()),
        (admin.clone(), grantee.clone(), role_name, timestamp),
    );
}

// ---------------------------------------------------------------------------
// NFT Marketplace event helpers
// ---------------------------------------------------------------------------

/// Topics: ("nft", "listed")
#[allow(clippy::too_many_arguments)]
pub fn emit_nft_listed(
    env: &Env,
    correlation_id: &BytesN<32>,
    listing_id: u64,
    seller: &Address,
    collection: &Address,
    token_id: u64,
    price: i128,
    currency: &Address,
    listed_at: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("listed"), correlation_id.clone()),
        (
            listing_id,
            seller.clone(),
            collection.clone(),
            token_id,
            price,
            currency.clone(),
            listed_at,
        ),
    );
}

/// Topics: ("nft", "sold")
pub fn emit_nft_sold(
    env: &Env,
    correlation_id: &BytesN<32>,
    listing_id: u64,
    seller: &Address,
    buyer: &Address,
    price: i128,
    sold_at: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("sold"), correlation_id.clone()),
        (listing_id, seller.clone(), buyer.clone(), price, sold_at),
    );
}

/// Topics: ("nft", "offer")
pub fn emit_nft_offer(
    env: &Env,
    correlation_id: &BytesN<32>,
    offer_id: u64,
    offerer: &Address,
    token_id: u64,
    amount: i128,
    expires_at: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("offer"), correlation_id.clone()),
        (offer_id, offerer.clone(), token_id, amount, expires_at),
    );
}

/// Topics: ("nft", "bid")
pub fn emit_nft_bid(
    env: &Env,
    correlation_id: &BytesN<32>,
    auction_id: u64,
    bidder: &Address,
    amount: i128,
    new_end: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("bid"), correlation_id.clone()),
        (auction_id, bidder.clone(), amount, new_end),
    );
}

/// Topics: ("nft", "auction")
pub fn emit_nft_auction(
    env: &Env,
    correlation_id: &BytesN<32>,
    auction_id: u64,
    seller: &Address,
    collection: &Address,
    token_id: u64,
    start_price: i128,
    end_time: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("auction"), correlation_id.clone()),
        (
            auction_id,
            seller.clone(),
            collection.clone(),
            token_id,
            start_price,
            end_time,
        ),
    );
}

/// Topics: ("nft", "settle")
pub fn emit_nft_settle(
    env: &Env,
    correlation_id: &BytesN<32>,
    auction_id: u64,
    winner: &Address,
    final_price: i128,
    settled_at: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("settle"), correlation_id.clone()),
        (auction_id, winner.clone(), final_price, settled_at),
    );
}

/// Topics: ("nft", "royal")
pub fn emit_royalty_paid(
    env: &Env,
    correlation_id: &BytesN<32>,
    token_id: u64,
    recipient: &Address,
    amount: i128,
    paid_at: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("royal"), correlation_id.clone()),
        (token_id, recipient.clone(), amount, paid_at),
    );
}

/// Topics: ("nft", "col_reg")
pub fn emit_collection_registered(
    env: &Env,
    correlation_id: &BytesN<32>,
    collection: &Address,
    admin: &Address,
    registered_at: u64,
) {
    env.events().publish(
        (symbol_short!("nft"), symbol_short!("col_reg"), correlation_id.clone()),
        (collection.clone(), admin.clone(), registered_at),
    );
}

/// Emits `RoleRevoked`.
///
/// Topics: `("role", "revoked")`
///
/// Data: `(admin, grantee, role_name, timestamp)`
pub fn emit_role_revoked(
    env: &Env,
    correlation_id: &BytesN<32>,
    admin: &Address,
    grantee: &Address,
    role_name: Symbol,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("role"), symbol_short!("revoked"), correlation_id.clone()),
        (admin.clone(), grantee.clone(), role_name, timestamp),
    );
}

/// Emits `ProposalCreated`.
///
/// Topics: `("proposal", "created")`
///
/// Data: `(proposal_id, proposer, action_description, timestamp)`
pub fn emit_proposal_created(
    env: &Env,
    correlation_id: &BytesN<32>,
    proposal_id: u64,
    proposer: &Address,
    action: Symbol,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("proposal"), symbol_short!("created"), correlation_id.clone()),
        (proposal_id, proposer.clone(), action, timestamp),
    );
}

/// Emits `ProposalApproved`.
///
/// Topics: `("proposal", "approved")`
///
/// Data: `(proposal_id, approver, approval_count, timestamp)`
pub fn emit_proposal_approved(
    env: &Env,
    correlation_id: &BytesN<32>,
    proposal_id: u64,
    approver: &Address,
    approval_count: u32,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("proposal"), symbol_short!("approved"), correlation_id.clone()),
        (proposal_id, approver.clone(), approval_count, timestamp),
    );
}

/// Emits `ProposalExecuted`.
///
/// Topics: `("proposal", "executed")`
///
/// Data: `(proposal_id, executor, approval_count, timestamp)`
pub fn emit_proposal_executed(
    env: &Env,
    correlation_id: &BytesN<32>,
    proposal_id: u64,
    executor: &Address,
    approval_count: u32,
    timestamp: u64,
) {
    env.events().publish(
        (symbol_short!("proposal"), symbol_short!("executed"), correlation_id.clone()),
        (proposal_id, executor.clone(), approval_count, timestamp),
    );
}

/// Emits `ImportSimulated`.
///
/// Topics: `("import", "sim")`
/// Data: `(total_rows, create_count, update_count, skip_count, error_count, batch_fingerprint)`
pub fn emit_import_simulated(
    env: &Env,
    total_rows: u32,
    create_count: u32,
    update_count: u32,
    skip_count: u32,
    error_count: u32,
    batch_fingerprint: &BytesN<32>,
) {
    env.events().publish(
        (symbol_short!("import"), symbol_short!("sim")),
        (
            total_rows,
            create_count,
            update_count,
            skip_count,
            error_count,
            batch_fingerprint.clone(),
        ),
    );
}

/// Emits `ImportCommitted`.
///
/// Topics: `("import", "commit")`
/// Data: `(caller, total_rows, create_count, update_count, skip_count, error_count, batch_fingerprint)`
pub fn emit_import_committed(
    env: &Env,
    caller: &Address,
    total_rows: u32,
    create_count: u32,
    update_count: u32,
    skip_count: u32,
    error_count: u32,
    batch_fingerprint: &BytesN<32>,
) {
    env.events().publish(
        (symbol_short!("import"), symbol_short!("commit")),
        (
            caller.clone(),
            total_rows,
            create_count,
            update_count,
            skip_count,
            error_count,
            batch_fingerprint.clone(),
        ),
    );
}

/// Emits `ImportFailed`.
///
/// Topics: `("import", "failed")`
/// Data: `(caller, total_rows, error_count, error_code)`
pub fn emit_import_failed(
    env: &Env,
    caller: &Address,
    total_rows: u32,
    error_count: u32,
    error_code: u32,
) {
    env.events().publish(
        (symbol_short!("import"), symbol_short!("failed")),
        (caller.clone(), total_rows, error_count, error_code),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        emit_action_executed, emit_aid_created, emit_aid_claimed, emit_aid_settled,
        emit_module_initialized, emit_permission_changed, normalize_correlation_id,
        validate_correlation_id, CorrelationIdError, CORRELATION_ID_MAX_LEN,
        CORRELATION_ID_MIN_LEN,
    };
    use soroban_sdk::{
        contract, contractimpl, symbol_short,
        testutils::{Address as _, Events},
        Address, BytesN, Env, FromVal, IntoVal, Symbol,
    };

    #[contract]
    struct EventTestContract;

    #[contractimpl]
    impl EventTestContract {
        pub fn publish_aid_created(
            env: Env,
            correlation_id: BytesN<32>,
            aid_id: u64,
            donor: Address,
            recipient: Address,
            amount: i128,
            created_at: u64,
            expires_at: u64,
        ) {
            emit_aid_created(
                &env, &correlation_id, aid_id, &donor, &recipient, amount, created_at, expires_at,
            );
        }

        pub fn publish_module_initialized(
            env: Env,
            correlation_id: BytesN<32>,
            module: Symbol,
            version: u32,
            caller: Address,
            initialized_at: u64,
        ) {
            emit_module_initialized(
                &env,
                &correlation_id,
                module,
                version,
                &caller,
                initialized_at,
            );
        }

        pub fn publish_action_executed(
            env: Env,
            correlation_id: BytesN<32>,
            module: Symbol,
            action: Symbol,
            caller: Address,
            success: bool,
            executed_at: u64,
        ) {
            emit_action_executed(
                &env,
                &correlation_id,
                module,
                action,
                &caller,
                success,
                executed_at,
            );
        }

        pub fn publish_permission_changed(
            env: Env,
            correlation_id: BytesN<32>,
            module: Symbol,
            role: Symbol,
            subject: Address,
            granted: bool,
            changed_at: u64,
        ) {
            emit_permission_changed(
                &env,
                &correlation_id,
                module,
                role,
                &subject,
                granted,
                changed_at,
            );
        }

        pub fn publish_aid_claimed(
            env: Env,
            correlation_id: BytesN<32>,
            aid_id: u64,
            claimant: Address,
            claimed_at: u64,
        ) {
            emit_aid_claimed(&env, &correlation_id, aid_id, &claimant, claimed_at);
        }

        pub fn publish_aid_settled(
            env: Env,
            correlation_id: BytesN<32>,
            aid_id: u64,
            recipient: Address,
            amount: i128,
            settled_at: u64,
        ) {
            emit_aid_settled(&env, &correlation_id, aid_id, &recipient, amount, settled_at);
        }
    }

    fn cid(env: &Env, s: &str) -> BytesN<32> {
        let mut buf = [0u8; 32];
        let bytes = s.as_bytes();
        assert!(bytes.len() <= 32, "test correlation id too long");
        buf[..bytes.len()].copy_from_slice(bytes);
        BytesN::from_array(env, &buf)
    }

    #[test]
    fn aid_created_has_stable_topics_and_data() {
        let env = Env::default();
        let donor = Address::generate(&env);
        let recipient = Address::generate(&env);
        let correlation_id = cid(&env, "wf-aid-0001");
        let contract_id = env.register_contract(None, EventTestContract);
        let client = EventTestContractClient::new(&env, &contract_id);

        client.publish_aid_created(
            &correlation_id,
            &7,
            &donor,
            &recipient,
            &500,
            &100,
            &1_000,
        );

        let events = env.events().all();
        assert_eq!(events.len(), 1);

        let (emitter, topics, data) = events.get(0).unwrap();

        assert_eq!(emitter, contract_id);
        assert_eq!(
            topics,
            (
                symbol_short!("aid"),
                symbol_short!("created"),
                correlation_id.clone(),
            )
                .into_val(&env)
        );
        let decoded_data: (u64, Address, Address, i128, u64, u64) = FromVal::from_val(&env, &data);

        assert_eq!(
            decoded_data,
            (7u64, donor, recipient, 500i128, 100u64, 1_000u64)
        );
    }

    #[test]
    fn module_initialized_has_stable_topics_and_data() {
        let env = Env::default();
        let module = symbol_short!("aid");
        let caller = Address::generate(&env);
        let correlation_id = cid(&env, "wf-init-0001");
        let contract_id = env.register_contract(None, EventTestContract);
        let client = EventTestContractClient::new(&env, &contract_id);

        client.publish_module_initialized(&correlation_id, &module, &1, &caller, &1_000);

        let events = env.events().all();
        assert_eq!(events.len(), 1);

        let (emitter, topics, data) = events.get(0).unwrap();

        assert_eq!(emitter, contract_id);
        assert_eq!(
            topics,
            (
                symbol_short!("logging"),
                symbol_short!("init"),
                correlation_id.clone(),
            )
                .into_val(&env)
        );
        let decoded_data: (Symbol, u32, Address, u64) = FromVal::from_val(&env, &data);

        assert_eq!(decoded_data, (module, 1, caller.clone(), 1_000));
    }

    #[test]
    fn action_executed_has_stable_topics_and_data() {
        let env = Env::default();
        let module = symbol_short!("aid");
        let action = symbol_short!("create");
        let caller = Address::generate(&env);
        let correlation_id = cid(&env, "wf-act-0001");
        let contract_id = env.register_contract(None, EventTestContract);
        let client = EventTestContractClient::new(&env, &contract_id);

        client.publish_action_executed(
            &correlation_id,
            &module,
            &action,
            &caller,
            &true,
            &1_000,
        );

        let events = env.events().all();
        assert_eq!(events.len(), 1);

        let (_emitter, topics, data) = events.get(0).unwrap();

        assert_eq!(
            topics,
            (
                symbol_short!("logging"),
                symbol_short!("action"),
                correlation_id.clone(),
            )
                .into_val(&env)
        );
        let decoded_data: (Symbol, Symbol, Address, bool, u64) = FromVal::from_val(&env, &data);

        assert_eq!(decoded_data, (module, action, caller.clone(), true, 1_000));
    }

    #[test]
    fn permission_changed_has_stable_topics_and_data() {
        let env = Env::default();
        let module = symbol_short!("treasury");
        let role = symbol_short!("manager");
        let subject = Address::generate(&env);
        let correlation_id = cid(&env, "wf-perm-0001");
        let contract_id = env.register_contract(None, EventTestContract);
        let client = EventTestContractClient::new(&env, &contract_id);

        client.publish_permission_changed(
            &correlation_id,
            &module,
            &role,
            &subject,
            &true,
            &1_000,
        );

        let events = env.events().all();
        assert_eq!(events.len(), 1);

        let (_emitter, topics, data) = events.get(0).unwrap();

        assert_eq!(
            topics,
            (
                symbol_short!("logging"),
                symbol_short!("perm"),
                correlation_id.clone(),
            )
                .into_val(&env)
        );
        let decoded_data: (Symbol, Symbol, Address, bool, u64) = FromVal::from_val(&env, &data);

        assert_eq!(decoded_data, (module, role, subject.clone(), true, 1_000));
    }

    #[test]
    fn validate_correlation_id_accepts_canonical_ids() {
        let env = Env::default();
        let id = cid(&env, "wf-2024-0001");
        assert_eq!(validate_correlation_id(&id), Ok(()));
    }

    #[test]
    fn validate_correlation_id_rejects_too_short() {
        let env = Env::default();
        let id = cid(&env, "abc");
        assert_eq!(
            validate_correlation_id(&id),
            Err(CorrelationIdError::TooShort)
        );
        assert!(CORRELATION_ID_MIN_LEN > 3);
    }

    #[test]
    fn validate_correlation_id_rejects_invalid_character() {
        let env = Env::default();
        let id = cid(&env, "wf id with space");
        assert_eq!(
            validate_correlation_id(&id),
            Err(CorrelationIdError::InvalidCharacter)
        );
    }

    #[test]
    fn validate_correlation_id_accepts_max_length() {
        let env = Env::default();
        let buf = [b'a'; 32];
        let id = BytesN::from_array(&env, &buf);
        assert_eq!(validate_correlation_id(&id), Ok(()));
    }

    #[test]
    fn normalize_correlation_id_rejects_invalid() {
        let env = Env::default();
        let id = cid(&env, "bad-id-with-space !");
        assert_eq!(
            normalize_correlation_id(&id),
            Err(CorrelationIdError::InvalidCharacter)
        );
    }

    #[test]
    fn normalize_correlation_id_passes_through_valid() {
        let env = Env::default();
        let id = cid(&env, "wf-valid-0001");
        let normalized = normalize_correlation_id(&id).unwrap();
        assert_eq!(normalized, id);
    }

    #[test]
    fn multi_step_workflow_shares_correlation_id() {
        let env = Env::default();
        let donor = Address::generate(&env);
        let recipient = Address::generate(&env);
        let correlation_id = cid(&env, "wf-multi-0001");
        let contract_id = env.register_contract(None, EventTestContract);
        let client = EventTestContractClient::new(&env, &contract_id);

        client.publish_aid_created(
            &correlation_id,
            &1,
            &donor,
            &recipient,
            &500,
            &100,
            &1_000,
        );
        client.publish_aid_claimed(&correlation_id, &1, &recipient, &200);
        client.publish_aid_settled(&correlation_id, &1, &recipient, &500, &300);

        let events = env.events().all();
        assert_eq!(events.len(), 3);

        for i in 0..events.len() {
            let (_emitter, topics, _data) = events.get(i).unwrap();
            let actual_cid: BytesN<32> = FromVal::from_val(&env, &topics.get(2).unwrap());
            assert_eq!(actual_cid, correlation_id);
        }
    }

    #[test]
    fn single_step_workflow_has_correlation_id() {
        let env = Env::default();
        let donor = Address::generate(&env);
        let recipient = Address::generate(&env);
        let correlation_id = cid(&env, "wf-single-0001");
        let contract_id = env.register_contract(None, EventTestContract);
        let client = EventTestContractClient::new(&env, &contract_id);

        client.publish_aid_created(
            &correlation_id,
            &1,
            &donor,
            &recipient,
            &500,
            &100,
            &1_000,
        );

        let events = env.events().all();
        assert_eq!(events.len(), 1);
        let (_emitter, topics, _data) = events.get(0).unwrap();
        let actual_cid: BytesN<32> = FromVal::from_val(&env, &topics.get(2).unwrap());
        assert_eq!(actual_cid, correlation_id);
    }

    #[test]
    fn distinct_workflows_have_distinct_correlation_ids() {
        let env = Env::default();
        let donor = Address::generate(&env);
        let recipient = Address::generate(&env);
        let cid_a = cid(&env, "wf-a-0001");
        let cid_b = cid(&env, "wf-b-0001");
        let contract_id = env.register_contract(None, EventTestContract);
        let client = EventTestContractClient::new(&env, &contract_id);

        client.publish_aid_created(&cid_a, &1, &donor, &recipient, &500, &100, &1_000);
        client.publish_aid_created(&cid_b, &2, &donor, &recipient, &600, &100, &1_000);

        let events = env.events().all();
        assert_eq!(events.len(), 2);
        let (_e0, t0, _d0) = events.get(0).unwrap();
        let (_e1, t1, _d1) = events.get(1).unwrap();
        let d0: BytesN<32> = FromVal::from_val(&env, &t0.get(2).unwrap());
        let d1: BytesN<32> = FromVal::from_val(&env, &t1.get(2).unwrap());
        assert_ne!(d0, d1);
        assert_eq!(d0, cid_a);
        assert_eq!(d1, cid_b);
    }
}
