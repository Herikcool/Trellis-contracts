//! Tamper-evident chronological ledger for privileged and financial events (Issue #233).
//!
//! Soroban already emits events, but emitted events are not individually
//! verifiable: an indexer that receives them cannot prove that one was not
//! inserted, removed, reordered, or edited before it reached them, and the
//! contract itself keeps no ordered record it can re-derive later. This module
//! adds an **append-only, hash-chained ledger** for the events that matter most
//! to an auditor — role/admin changes, configuration changes, upgrades, pauses
//! and every movement of value — plus a bounded, verifiable export.
//!
//! ## Design
//!
//! | Property | Mechanism |
//! |---|---|
//! | Chronological | `seq` is strictly `previous + 1`; ledger sequence and timestamp must be non-decreasing. |
//! | Tamper-evident | `hash = sha256(domain ‖ schema ‖ seq ‖ event fields ‖ prev_hash)`; any edit changes every later hash. |
//! | Append-only | Entries are only ever written for `seq = length + 1`; there is no update or delete API. |
//! | Verifiable | [`verify_chain`] re-derives the full chain from storage and compares it with the stored head. |
//! | Exportable | [`export_ledger`] returns a bounded, versioned bundle whose entries and digest can be checked off-chain with [`verify_export`]. |
//! | Compatible | Purely additive: new storage keys, no changes to existing event topics, records, or entry points. |
//!
//! ## Hash definition
//!
//! Every field of [`AuditLedgerEntry`] except `hash` participates in the digest,
//! in a fixed order, encoded with the Soroban canonical `Val` XDR form
//! (`ToXdr`). The domain separator [`AUDIT_DOMAIN`] and
//! [`AUDIT_LEDGER_SCHEMA_VERSION`] are mixed in first so an audit hash can never
//! collide with a hash produced by another subsystem or a future schema.
//!
//! ```text
//! hash(n) = sha256( xdr([
//!     "trellis_audit_ledger", schema_version,
//!     seq, class, kind, module, actor, subject, resource, attribute,
//!     amount, previous_amount, note, correlation_id,
//!     ledger, timestamp, prev_hash
//! ]) )
//! ```
//!
//! `prev_hash` for the first entry is the all-zero [`genesis_hash`].
//!
//! ## Failure model
//!
//! [`verify_chain`] reports the **first** failure and its sequence number rather
//! than returning a bare boolean, so an operator can decide how much prefix is
//! trustworthy. See `docs/AUDIT_LEDGER.md` for the runbook: what each failure
//! means, how to bound the blast radius, and how to recover.
//!
//! ## Operational limits
//!
//! - Verification is bounded to [`MAX_VERIFY_ENTRIES`] entries per call. An
//!   oversized ledger returns [`AuditVerificationFailure::ScanLimitExceeded`]
//!   instead of running unbounded work.
//! - Exports are paged: at most [`MAX_EXPORT_LIMIT`] entries per bundle, with a
//!   `from_seq` cursor and `has_more` flag.
//! - Entry storage uses persistent storage with the workspace TTL policy; the
//!   head uses instance storage because it is contract-level configuration.
//!
//! ## Compatibility
//!
//! This module is opt-in. Contracts adopt it by calling [`append_event`] on the
//! privileged/financial paths they already audit. Nothing in the existing
//! `shared::events`, `shared::timeline`, or contract interfaces is changed.
//!
//! ## Duplicate detection and merge review
//!
//! The ledger also backs a deterministic duplicate-detection and merge-review
//! workflow for records that may be entered more than once (aid records,
//! referrals, marketplace listings, oracle feeds).
//!
//! - [`candidate_fingerprint`] derives a stable 32-byte fingerprint from the
//!   identifying fields of a candidate. It is a pure function of its inputs, so
//!   two callers observing the same record always compute the same fingerprint.
//! - [`detect_duplicates`] scans the ledger for entries whose fingerprint
//!   matches, returning matches in ascending `seq` order. The scan is bounded by
//!   [`MAX_VERIFY_ENTRIES`] and is therefore reproducible.
//! - [`propose_merge`] records a reviewable merge proposal as an audited
//!   [`AuditEventKind::PrivilegedAction`] entry. Proposals are never applied
//!   automatically; a privileged actor must call [`approve_merge`] or
//!   [`reject_merge`].
//! - [`approve_merge`] marks a proposal approved and appends an audit entry.
//!   Approval is reversible via [`revert_merge`] while the proposal is still
//!   within the revert window, and every transition is audited.
//!
//! All merge state lives under new storage keys, so existing public APIs,
//! schemas, and user-facing behavior are unchanged.

use soroban_sdk::{
    contracttype, symbol_short, xdr::ToXdr, Address, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

use crate::auth::{require_permission, Permission};
use crate::errors::Error;
use crate::storage::{instance_get, instance_set, persistent_get, persistent_set};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Current audit-ledger schema version. Bump only with a documented migration;
/// [`AUDIT_DOMAIN`] and this version are part of every entry hash.
pub const AUDIT_LEDGER_SCHEMA_VERSION: u32 = 1;

/// Domain separator mixed into every hash. Prevents cross-subsystem replay of
/// an audit hash as some other kind of digest.
pub const AUDIT_DOMAIN: &str = "trellis_audit_ledger";

/// Page size used when an export caller passes `0`.
pub const DEFAULT_EXPORT_LIMIT: u32 = 20;

/// Largest number of entries one export may return.
pub const MAX_EXPORT_LIMIT: u32 = 100;

/// Largest number of entries one verification pass will scan.
pub const MAX_VERIFY_ENTRIES: u64 = 10_000;

/// Domain separator mixed into duplicate fingerprints. Kept distinct from
/// [`AUDIT_DOMAIN`] so a fingerprint can never be confused with an entry hash.
pub const DUPLICATE_DOMAIN: &str = "trellis_duplicate_fingerprint";

/// Schema version for duplicate fingerprints. Bump only with a documented
/// migration; it is part of every fingerprint.
pub const DUPLICATE_SCHEMA_VERSION: u32 = 1;

/// Maximum number of duplicate candidates one detection pass will return.
pub const MAX_DUPLICATE_CANDIDATES: u32 = 50;

/// Number of ledgers after approval during which a merge may be reverted.
pub const MERGE_REVERT_WINDOW_LEDGERS: u32 = 17_280;

// ---------------------------------------------------------------------------
// Event taxonomy
// ---------------------------------------------------------------------------

/// Coarse class of an audited event.
///
/// The class is stored separately from [`AuditEventKind`] so an indexer can
/// filter "everything financial" without matching a growing list of kinds. It
/// is derived from the kind by [`AuditEventKind::class`] and always recorded,
/// never supplied by the caller.
#[contracttype]
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditEventClass {
    /// Privileged/administrative action: roles, config, upgrades, pause state.
    Privileged = 1,
    /// Movement or commitment of value: treasury, aid, escrow, commissions.
    Financial = 2,
}

/// The privileged and financial events the ledger is designed to cover.
///
/// Variants are stable: add new variants with new numeric codes, never reuse or
/// renumber an existing one. Codes are part of the entry hash.
#[contracttype]
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditEventKind {
    // Privileged ---------------------------------------------------------
    /// A role was granted to an address.
    RoleGranted = 1,
    /// A role was revoked from an address.
    RoleRevoked = 2,
    /// Contract or module configuration changed.
    ConfigChanged = 3,
    /// The contract or a scoped module was paused.
    ContractPaused = 4,
    /// The contract or a scoped module was resumed.
    ContractResumed = 5,
    /// A contract upgrade was executed.
    ContractUpgraded = 6,
    /// A data/schema migration step was applied.
    MigrationApplied = 7,
    /// Quota usage was reset by an administrator.
    QuotaReset = 8,
    /// A reserve strategy adapter was approved.
    StrategyApproved = 9,
    /// A reserve strategy adapter was revoked.
    StrategyRevoked = 10,
    /// An emergency stop/override was exercised by an administrator.
    EmergencyAction = 11,
    /// A privileged action that does not fit a more specific kind.
    PrivilegedAction = 12,

    // Financial ----------------------------------------------------------
    /// An aid record was created and funded.
    AidCreated = 20,
    /// An aid record was claimed.
    AidClaimed = 21,
    /// An aid record expired and was refunded.
    AidRefunded = 22,
    /// Funds were deposited into a treasury category.
    TreasuryDeposit = 30,
    /// Funds were withdrawn from a treasury category.
    TreasuryWithdrawal = 31,
    /// Funds left the treasury outside the normal withdrawal path.
    TreasuryEmergencyWithdrawal = 32,
    /// A previously scheduled treasury withdrawal executed.
    TreasuryScheduledWithdrawal = 33,
    /// An escrow was funded.
    EscrowCreated = 40,
    /// Escrow funds were released to the beneficiary.
    EscrowReleased = 41,
    /// Escrow funds were returned to the payer.
    EscrowRefunded = 42,
    /// A referral commission was paid.
    CommissionPaid = 50,
    /// A referral relationship was registered.
    ReferralRegistered = 51,
    /// A governance action moved value or changed a financial parameter.
    GovernanceFinancialAction = 60,
    /// An oracle feed value that gates financial execution changed.
    OracleFeedUpdated = 70,
    /// A marketplace listing settled value between parties.
    MarketplaceSettled = 80,
    /// A financial action that does not fit a more specific kind.
    FinancialAction = 81,

    // Duplicate / merge review -------------------------------------------
    /// A duplicate merge was proposed for review.
    MergeProposed = 90,
    /// A duplicate merge proposal was approved.
    MergeApproved = 91,
    /// A duplicate merge proposal was rejected.
    MergeRejected = 92,
    /// An approved duplicate merge was reverted.
    MergeReverted = 93,
}

impl AuditEventKind {
    /// Returns the stable numeric code recorded in the entry and its hash.
    pub fn code(self) -> u32 {
        self as u32
    }

    /// Returns the coarse class this kind belongs to.
    pub fn class(self) -> AuditEventClass {
        match self {
            AuditEventKind::RoleGranted
            | AuditEventKind::RoleRevoked
            | AuditEventKind::ConfigChanged
            | AuditEventKind::ContractPaused
            | AuditEventKind::ContractResumed
            | AuditEventKind::ContractUpgraded
            | AuditEventKind::MigrationApplied
            | AuditEventKind::QuotaReset
            | AuditEventKind::StrategyApproved
            | AuditEventKind::StrategyRevoked
            | AuditEventKind::EmergencyAction
            | AuditEventKind::PrivilegedAction
            | AuditEventKind::MergeProposed
            | AuditEventKind::MergeApproved
            | AuditEventKind::MergeRejected
            | AuditEventKind::MergeReverted => AuditEventClass::Privileged,
            _ => AuditEventClass::Financial,
        }
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Fields supplied by the caller when appending an event.
///
/// Only non-sensitive, indexable values belong here. Free-form text is
/// deliberately not accepted: `note`, when present, must be a short reason or
/// action code (`Symbol`), never a memo, name, or address rendered as text.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEventParams {
    /// What happened.
    pub kind: AuditEventKind,
    /// Which contract/module the event belongs to, e.g. `treasury`.
    pub module: Symbol,
    /// The authenticated actor that performed the action.
    pub actor: Address,
    /// Optional subject of the action (role holder, recipient, beneficiary).
    pub subject: Option<Address>,
    /// Optional resource affected (token, strategy, feed).
    pub resource: Option<Address>,
    /// Optional stable attribute (category, role, scope).
    pub attribute: Option<Symbol>,
    /// Optional amount moved or committed.
    pub amount: Option<i128>,
    /// Optional amount/balance immediately before the action.
    pub previous_amount: Option<i128>,
    /// Optional short reason/action code.
    pub note: Option<Symbol>,
    /// Caller correlation id, carried through for indexer joins. The all-zero
    /// value means "no correlation id" (`Option<BytesN>` is not encodable in a
    /// Soroban contract type).
    pub correlation_id: BytesN<32>,
}

/// One immutable entry in the audit ledger.
///
/// Entries are written exactly once. Any field change — including a change to
/// an earlier entry — invalidates that entry's `hash`, every later `prev_hash`
/// link, and the stored head.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditLedgerEntry {
    /// Strictly increasing position, starting at 1.
    pub seq: u64,
    /// Schema version at the time the entry was written.
    pub schema_version: u32,
    /// Derived class of `kind`.
    pub class: AuditEventClass,
    /// What happened.
    pub kind: AuditEventKind,
    /// Contract/module the event belongs to.
    pub module: Symbol,
    /// Authenticated actor.
    pub actor: Address,
    /// Optional subject.
    pub subject: Option<Address>,
    /// Optional resource.
    pub resource: Option<Address>,
    /// Optional attribute.
    pub attribute: Option<Symbol>,
    /// Optional amount.
    pub amount: Option<i128>,
    /// Optional prior amount/balance.
    pub previous_amount: Option<i128>,
    /// Optional short reason/action code.
    pub note: Option<Symbol>,
    /// Correlation id, or the all-zero hash when none was supplied.
    pub correlation_id: BytesN<32>,
    /// Ledger sequence at which the entry was written.
    pub ledger: u32,
    /// Ledger timestamp at which the entry was written.
    pub timestamp: u64,
    /// Hash of the previous entry, or [`genesis_hash`] for `seq == 1`.
    pub prev_hash: BytesN<32>,
    /// This entry's hash.
    pub hash: BytesN<32>,
}

/// Current head of the ledger.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditLedgerHead {
    /// Schema version the ledger was created with.
    pub schema_version: u32,
    /// Number of committed entries (also the sequence of the newest entry).
    pub length: u64,
    /// Hash of the newest entry, or [`genesis_hash`] when empty.
    pub head_hash: BytesN<32>,
    /// Ledger sequence of the newest entry, `0` when empty.
    pub last_ledger: u32,
    /// Ledger timestamp of the newest entry, `0` when empty.
    pub last_timestamp: u64,
}

/// Storage keys owned by this module.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditLedgerKey {
    /// Ledger head (instance storage).
    Head,
    /// Entry by sequence number (persistent storage).
    Entry(u64),
}

/// Why verification failed.
#[contracttype]
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditVerificationFailure {
    /// Chain is intact.
    None = 0,
    /// A sequence number in `1..=length` has no stored entry.
    MissingEntry = 1,
    /// A stored entry's `seq` field does not match its storage key.
    NonMonotonicSequence = 2,
    /// A stored entry's recomputed hash differs from its `hash` field.
    HashMismatch = 3,
    /// An entry's `prev_hash` does not match the previous entry's hash.
    BrokenLink = 4,
    /// The stored head does not match the re-derived chain.
    HeadMismatch = 5,
    /// An entry exists beyond the stored head length.
    UnexpectedTail = 6,
    /// Ledger sequence moved backwards.
    LedgerRegression = 7,
    /// Timestamp moved backwards within the same ledger.
    TimestampRegression = 8,
    /// An exported bundle's digest does not match its contents.
    ExportDigestMismatch = 9,
    /// An exported bundle's range/head metadata is inconsistent.
    ExportMetadataMismatch = 10,
    /// The ledger is larger than [`MAX_VERIFY_ENTRIES`].
    ScanLimitExceeded = 11,
}

/// Result of [`verify_chain`] or [`verify_export`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditVerificationReport {
    /// Schema version used for verification.
    pub schema_version: u32,
    /// `true` only when every checked entry and the head are consistent.
    pub verified: bool,
    /// Number of entries checked.
    pub entries_checked: u64,
    /// Re-derived head hash (for a full chain) or the export head hash.
    pub head_hash: BytesN<32>,
    /// Sequence of the first failing entry/check, `0` when verified.
    pub first_invalid_seq: u64,
    /// Failure kind, [`AuditVerificationFailure::None`] when verified.
    pub failure: AuditVerificationFailure,
}

/// Bounded, versioned export of a contiguous ledger range.
///
/// The bundle is self-contained for its range: `start_prev_hash` anchors the
/// first entry to the chain that precedes it, each entry carries its own hash,
/// and `digest` covers the bundle metadata plus every entry. A consumer can
/// verify it without contract storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditLedgerExport {
    /// Schema version of this bundle.
    pub schema_version: u32,
    /// First sequence number included (or `length + 1` for an empty tail).
    pub from_seq: u64,
    /// Last sequence number included (`from_seq - 1` when empty).
    pub to_seq: u64,
    /// Ledger length at export time.
    pub ledger_length: u64,
    /// Ledger head hash at export time.
    pub head_hash: BytesN<32>,
    /// Hash preceding the first exported entry.
    pub start_prev_hash: BytesN<32>,
    /// Contiguous entries in sequence order.
    pub entries: Vec<AuditLedgerEntry>,
    /// `sha256` over the bundle metadata and entries.
    pub digest: BytesN<32>,
    /// `true` when entries remain after `to_seq`.
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// The all-zero hash used as `prev_hash` for the first entry.
pub fn genesis_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0u8; 32])
}

fn domain_symbol(env: &Env) -> Symbol {
    Symbol::new(env, AUDIT_DOMAIN)
}

fn hash_parts(env: &Env, parts: &Vec<Val>) -> BytesN<32> {
    env.crypto().sha256(&parts.clone().to_xdr(env)).to_bytes()
}

fn entry_hash_parts(env: &Env, entry: &AuditLedgerEntry) -> Vec<Val> {
    let mut parts: Vec<Val> = Vec::new(env);
    parts.push_back(domain_symbol(env).into_val(env));
    parts.push_back(entry.schema_version.into_val(env));
    parts.push_back(entry.seq.into_val(env));
    parts.push_back(entry.class.into_val(env));
    parts.push_back(entry.kind.into_val(env));
    parts.push_back(entry.module.clone().into_val(env));
    parts.push_back(entry.actor.clone().into_val(env));
    parts.push_back(entry.subject.clone().into_val(env));
    parts.push_back(entry.resource.clone().into_val(env));
    parts.push_back(entry.attribute.clone().into_val(env));
    parts.push_back(entry.amount.into_val(env));
    parts.push_back(entry.previous_amount.into_val(env));
    parts.push_back(entry.note.clone().into_val(env));
    parts.push_back(entry.correlation_id.clone().into_val(env));
    parts.push_back(entry.ledger.into_val(env));
    parts.push_back(entry.timestamp.into_val(env));
    parts.push_back(entry.prev_hash.clone().into_val(env));
    parts
}

/// Recomputes the hash of `entry` over every field except `entry.hash`.
///
/// This is the function an off-chain verifier mirrors. It is deterministic and
/// independent of storage.
pub fn entry_hash(env: &Env, entry: &AuditLedgerEntry) -> BytesN<32> {
    hash_parts(env, &entry_hash_parts(env, entry))
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

fn empty_head(env: &Env) -> AuditLedgerHead {
    AuditLedgerHead {
        schema_version: AUDIT_LEDGER_SCHEMA_VERSION,
        length: 0,
        head_hash: genesis_hash(env),
        last_ledger: 0,
        last_timestamp: 0,
    }
}

/// Returns the current head, or an empty head when the ledger has not been
/// written yet. Reading never creates state.
pub fn head(env: &Env) -> AuditLedgerHead {
    instance_get::<AuditLedgerKey, AuditLedgerHead>(env, &AuditLedgerKey::Head)
        .unwrap_or_else(|| empty_head(env))
}

/// Number of committed entries. Zero until the first append.
pub fn length(env: &Env) -> u64 {
    head(env).length
}

/// Returns the entry at `seq`, if it exists. Bumps the entry's TTL on read.
pub fn get_entry(env: &Env, seq: u64) -> Option<AuditLedgerEntry> {
    if seq == 0 {
        return None;
    }
    persistent_get::<AuditLedgerKey, AuditLedgerEntry>(env, &AuditLedgerKey::Entry(seq))
}

// ---------------------------------------------------------------------------
// Append
// ---------------------------------------------------------------------------

/// Appends one privileged or financial event to the ledger.
///
/// # Ordering
///
/// The caller-supplied actor must already have been authorized by the calling
/// contract; this function records, it does not authorize. Appends must be
/// chronological with respect to the ledger head:
///
/// - the new ledger sequence may not be lower than the head's;
/// - within the same ledger sequence, the timestamp may not decrease.
///
/// Violations return [`Error::InvalidArgument`] and write nothing, so a
/// caller cannot create a fork or back-date an event.
///
/// # Atomicity
///
/// The entry is written first, then the head. Soroban rolls back the whole
/// invocation if either write fails, so a partially-appended entry cannot be
/// observed by another transaction. Re-entrancy is not possible because the
/// host serializes contract invocations.
pub fn append_event(env: &Env, params: AuditEventParams) -> Result<AuditLedgerEntry, Error> {
    let current = head(env);
    let ledger = env.ledger().sequence();
    let timestamp = env.ledger().timestamp();

    if ledger < current.last_ledger {
        return Err(Error::InvalidArgument);
    }
    if ledger == current.last_ledger && timestamp < current.last_timestamp {
        return Err(Error::InvalidArgument);
    }

    let seq = current.length.checked_add(1).ok_or(Error::Overflow)?;
    let prev_hash = if current.length == 0 {
        genesis_hash(env)
    } else {
        current.head_hash.clone()
    };

    let mut entry = AuditLedgerEntry {
        seq,
        schema_version: AUDIT_LEDGER_SCHEMA_VERSION,
        class: params.kind.class(),
        kind: params.kind,
        module: params.module,
        actor: params.actor,
        subject: params.subject,
        resource: params.resource,
        attribute: params.attribute,
        amount: params.amount,
        previous_amount: params.previous_amount,
        note: params.note,
        correlation_id: params.correlation_id,
        ledger,
        timestamp,
        prev_hash,
        hash: genesis_hash(env),
    };
    entry.hash = entry_hash(env, &entry);

    persistent_set(env, &AuditLedgerKey::Entry(seq), &entry);
    instance_set(
        env,
        &AuditLedgerKey::Head,
        &AuditLedgerHead {
            schema_version: AUDIT_LEDGER_SCHEMA_VERSION,
            length: seq,
            head_hash: entry.hash.clone(),
            last_ledger: ledger,
            last_timestamp: timestamp,
        },
    );

    Ok(entry)
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

fn fail(
    _env: &Env,
    checked: u64,
    head_hash: BytesN<32>,
    seq: u64,
    failure: AuditVerificationFailure,
) -> AuditVerificationReport {
    AuditVerificationReport {
        schema_version: AUDIT_LEDGER_SCHEMA_VERSION,
        verified: false,
        entries_checked: checked,
        head_hash,
        first_invalid_seq: seq,
        failure,
    }
}

/// Re-derives the entire chain from storage and checks it against the head.
///
/// Detects, in this order per entry: missing entries, sequence/key mismatch,
/// hash edits, broken `prev_hash` links, ledger/timestamp regressions, an
/// unexpected entry beyond the head, and a head that does not match the
/// re-derived chain.
///
/// The scan is bounded by [`MAX_VERIFY_ENTRIES`]; larger ledgers return
/// [`AuditVerificationFailure::ScanLimitExceeded`]. Split verification by
/// exporting and checking ranges instead.
pub fn verify_chain(env: &Env) -> AuditVerificationReport {
    let current = head(env);
    let genesis = genesis_hash(env);

    if current.length > MAX_VERIFY_ENTRIES {
        return fail(
            env,
            0,
            current.head_hash.clone(),
            0,
            AuditVerificationFailure::ScanLimitExceeded,
        );
    }

    let mut running = genesis.clone();
    let mut checked: u64 = 0;
    let mut last_ledger: u32 = 0;
    let mut last_timestamp: u64 = 0;

    let mut seq: u64 = 1;
    while seq <= current.length {
        let Some(entry) = get_entry(env, seq) else {
            return fail(
                env,
                checked,
                running,
                seq,
                AuditVerificationFailure::MissingEntry,
            );
        };

        if entry.seq != seq {
            return fail(
                env,
                checked,
                running,
                seq,
                AuditVerificationFailure::NonMonotonicSequence,
            );
        }

        if entry.hash != entry_hash(env, &entry) {
            return fail(
                env,
                checked,
                running,
                seq,
                AuditVerificationFailure::HashMismatch,
            );
        }

        if entry.prev_hash != running {
            return fail(
                env,
                checked,
                running,
                seq,
                AuditVerificationFailure::BrokenLink,
            );
        }

        if entry.ledger < last_ledger {
            return fail(
                env,
                checked,
                running,
                seq,
                AuditVerificationFailure::LedgerRegression,
            );
        }

        if entry.ledger == last_ledger && entry.timestamp < last_timestamp {
            return fail(
                env,
                checked,
                running,
                seq,
                AuditVerificationFailure::TimestampRegression,
            );
        }

        running = entry.hash;
        last_ledger = entry.ledger;
        last_timestamp = entry.timestamp;
        checked = seq;
        seq += 1;
    }

    // An entry written past the committed head means the head was rolled back
    // or an entry was inserted without updating it.
    if get_entry(env, current.length + 1).is_some() {
        return fail(
            env,
            checked,
            running,
            current.length + 1,
            AuditVerificationFailure::UnexpectedTail,
        );
    }

    if running != current.head_hash {
        return fail(
            env,
            checked,
            running,
            current.length,
            AuditVerificationFailure::HeadMismatch,
        );
    }

    AuditVerificationReport {
        schema_version: AUDIT_LEDGER_SCHEMA_VERSION,
        verified: true,
        entries_checked: checked,
        head_hash: running,
        first_invalid_seq: 0,
        failure: AuditVerificationFailure::None,
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Recomputes the digest of an exported bundle.
///
/// Public so an off-chain verifier can reproduce [`AuditLedgerExport::digest`]
/// exactly, including after re-signing or re-serializing the bundle. Covers the
/// schema version, range metadata, head, start anchor, `has_more`, and every
/// entry.
pub fn export_digest(env: &Env, export: &AuditLedgerExport) -> BytesN<32> {
    let mut parts: Vec<Val> = Vec::new(env);
    parts.push_back(domain_symbol(env).into_val(env));
    parts.push_back(AUDIT_LEDGER_SCHEMA_VERSION.into_val(env));
    parts.push_back(export.from_seq.into_val(env));
    parts.push_back(export.to_seq.into_val(env));
    parts.push_back(export.ledger_length.into_val(env));
    parts.push_back(export.head_hash.clone().into_val(env));
    parts.push_back(export.start_prev_hash.clone().into_val(env));
    parts.push_back(export.has_more.into_val(env));
    parts.push_back(export.entries.clone().into_val(env));
    hash_parts(env, &parts)
}

/// Exports a contiguous range of the ledger for a maintainer.
///
/// `reader` must hold [`Permission::ReadAuditTrail`] (admin) and authorize the
/// call. At most `limit` entries are returned, capped at [`MAX_EXPORT_LIMIT`].
///
/// `from_seq` must be in `1..=length + 1`; `length + 1` yields an empty bundle
/// that anchors at the head, which is useful for "catch up" clients.
pub fn export_ledger(
    env: &Env,
    reader: &Address,
    from_seq: u64,
    limit: u32,
) -> Result<AuditLedgerExport, Error> {
    require_permission(env, reader, Permission::ReadAuditTrail)?;

    let current = head(env);
    if from_seq == 0 || from_seq > current.length + 1 {
        return Err(Error::InvalidArgument);
    }

    let page = if limit == 0 {
        DEFAULT_EXPORT_LIMIT
    } else if limit > MAX_EXPORT_LIMIT {
        MAX_EXPORT_LIMIT
    } else {
        limit
    };

    let mut entries: Vec<AuditLedgerEntry> = Vec::new(env);
    let start_prev_hash = if from_seq == 1 {
        genesis_hash(env)
    } else if from_seq == current.length + 1 {
        current.head_hash.clone()
    } else {
        match get_entry(env, from_seq - 1) {
            Some(entry) => entry.hash,
            None => return Err(Error::NotFound),
        }
    };

    let mut seq = from_seq;
    let mut to_seq = from_seq - 1;
    while seq <= current.length && entries.len() < page {
        match get_entry(env, seq) {
            Some(entry) => {
                entries.push_back(entry);
                to_seq = seq;
                seq += 1;
            }
            None => return Err(Error::NotFound),
        }
    }

    let mut export = AuditLedgerExport {
        schema_version: AUDIT_LEDGER_SCHEMA_VERSION,
        from_seq,
        to_seq,
        ledger_length: current.length,
        head_hash: current.head_hash,
        start_prev_hash,
        entries,
        digest: genesis_hash(env),
        has_more: seq <= current.length,
    };
    export.digest = export_digest(env, &export);
    Ok(export)
}

/// Verifies an exported bundle without reading contract storage.
///
/// Checks the bundle digest, schema version, range metadata, entry hashes,
/// chain links, chronological ordering, and — when the bundle reaches the head
/// — the head hash. This is the check an off-chain auditor runs after fetching
/// a bundle from an indexer or archive.
pub fn verify_export(env: &Env, export: &AuditLedgerExport) -> AuditVerificationReport {
    let genesis = genesis_hash(env);

    if export.schema_version != AUDIT_LEDGER_SCHEMA_VERSION {
        return fail(
            env,
            0,
            export.head_hash.clone(),
            export.from_seq,
            AuditVerificationFailure::ExportMetadataMismatch,
        );
    }

    if export.digest != export_digest(env, export) {
        return fail(
            env,
            0,
            export.head_hash.clone(),
            export.from_seq,
            AuditVerificationFailure::ExportDigestMismatch,
        );
    }

    let empty = export.entries.len() == 0;
    let expected_count = if empty {
        0
    } else {
        export
            .to_seq
            .saturating_sub(export.from_seq)
            .saturating_add(1)
    };

    if (empty && export.to_seq + 1 != export.from_seq)
        || (!empty && export.entries.len() as u64 != expected_count)
        || export.from_seq == 0
        || export.to_seq > export.ledger_length
        || export.from_seq > export.ledger_length + 1
    {
        return fail(
            env,
            export.entries.len() as u64,
            export.head_hash.clone(),
            export.from_seq,
            AuditVerificationFailure::ExportMetadataMismatch,
        );
    }

    let mut running = export.start_prev_hash.clone();
    let mut checked: u64 = 0;
    let mut last_ledger: u32 = 0;
    let mut last_timestamp: u64 = 0;
    let mut expected_seq = export.from_seq;

    for i in 0..export.entries.len() {
        let entry = export.entries.get(i).unwrap();

        if entry.seq != expected_seq {
            return fail(
                env,
                checked,
                running,
                entry.seq,
                AuditVerificationFailure::NonMonotonicSequence,
            );
        }
        if entry.hash != entry_hash(env, &entry) {
            return fail(
                env,
                checked,
                running,
                entry.seq,
                AuditVerificationFailure::HashMismatch,
            );
        }
        if entry.prev_hash != running {
            return fail(
                env,
                checked,
                running,
                entry.seq,
                AuditVerificationFailure::BrokenLink,
            );
        }
        if entry.ledger < last_ledger {
            return fail(
                env,
                checked,
                running,
                entry.seq,
                AuditVerificationFailure::LedgerRegression,
            );
        }
        if entry.ledger == last_ledger && entry.timestamp < last_timestamp {
            return fail(
                env,
                checked,
                running,
                entry.seq,
                AuditVerificationFailure::TimestampRegression,
            );
        }

        running = entry.hash;
        last_ledger = entry.ledger;
        last_timestamp = entry.timestamp;
        checked = entry.seq;
        expected_seq += 1;
    }

    // The bundle claims the head hash, so a non-empty bundle that reaches the
    // head must end on it; an empty tail bundle must itself be anchored there.
    let reaches_head = export.to_seq == export.ledger_length;
    if reaches_head {
        let anchor = if empty {
            &export.start_prev_hash
        } else {
            &running
        };
        if *anchor != export.head_hash {
            return fail(
                env,
                checked,
                running,
                export.to_seq,
                AuditVerificationFailure::HeadMismatch,
            );
        }
    } else if export.head_hash == genesis {
        return fail(
            env,
            checked,
            running,
            export.to_seq,
            AuditVerificationFailure::ExportMetadataMismatch,
        );
    }

    AuditVerificationReport {
        schema_version: AUDIT_LEDGER_SCHEMA_VERSION,
        verified: true,
        entries_checked: checked,
        head_hash: running,
        first_invalid_seq: 0,
        failure: AuditVerificationFailure::None,
    }
}

/// Checkpoint helper for off-chain anchoring.
///
/// Returns the head (length + hash) that a verifier can publish or persist. A
/// later [`verify_chain`] that reproduces this hash proves the on-chain ledger
/// is the same one the checkpoint was taken from; any edit to the prefix
/// changes the checkpoint.
pub fn checkpoint(env: &Env) -> AuditLedgerHead {
    head(env)
}

// ---------------------------------------------------------------------------
// Small helpers used by contracts
// ---------------------------------------------------------------------------

/// Convenience wrapper for privileged events (roles, config, upgrades, pause).
pub fn append_privileged(
    env: &Env,
    kind: AuditEventKind,
    module: Symbol,
    actor: Address,
    subject: Option<Address>,
    attribute: Option<Symbol>,
    note: Option<Symbol>,
) -> Result<AuditLedgerEntry, Error> {
    if kind.class() != AuditEventClass::Privileged {
        return Err(Error::InvalidArgument);
    }
    append_event(
        env,
        AuditEventParams {
            kind,
            module,
            actor,
            subject,
            resource: None,
            attribute,
            amount: None,
            previous_amount: None,
            note,
            correlation_id: genesis_hash(env),
        },
    )
}

/// Convenience wrapper for financial events.
#[allow(clippy::too_many_arguments)]
pub fn append_financial(
    env: &Env,
    kind: AuditEventKind,
    module: Symbol,
    actor: Address,
    subject: Option<Address>,
    resource: Option<Address>,
    attribute: Option<Symbol>,
    amount: Option<i128>,
    previous_amount: Option<i128>,
) -> Result<AuditLedgerEntry, Error> {
    if kind.class() != AuditEventClass::Financial {
        return Err(Error::InvalidArgument);
    }
    append_event(
        env,
        AuditEventParams {
            kind,
            module,
            actor,
            subject,
            resource,
            attribute,
            amount,
            previous_amount,
            note: None,
            correlation_id: genesis_hash(env),
        },
    )
}

/// Emits the head checkpoint as a ledger event, so indexers can anchor the
/// chain without an extra RPC read.
///
/// Topic is stable: `("audit", "head")`. Payload:
/// `(u64 length, BytesN<32> head_hash, u32 last_ledger, u64 last_timestamp)`.
pub fn publish_head_checkpoint(env: &Env) -> AuditLedgerHead {
    let current = head(env);
    env.events().publish(
        (symbol_short!("audit"), symbol_short!("head")),
        (
            current.length,
            current.head_hash.clone(),
            current.last_ledger,
            current.last_timestamp,
        ),
    );
    current
}
