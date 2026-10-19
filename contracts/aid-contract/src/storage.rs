//! Aid-contract storage layer.
//!
//! All persistent reads and writes are routed through the shared storage
//! helpers so that TTL bumps are applied consistently on every access.
//!
//! ## Key layout
//!
//! | Key                    | Storage type | Rationale                                      |
//! |------------------------|--------------|------------------------------------------------|
//! | `Aid(id)`              | persistent   | Long-lived financial record; must outlive many |
//! |                        |              | ledger closures until claimed/refunded.        |
//! | `AidCounter`           | instance     | Config-level counter; always needed when live. |
//! | `Token`                | instance     | Escrow token configured at initialisation.     |
//! | `DonorIndex(donor)`    | persistent   | Append-only aid-ID list per donor; grows with  |
//! |                        |              | the donor's history and must persist forever.  |
//! | `RecipientIndex(addr)` | persistent   | Append-only aid-ID list per recipient.         |
//! | `Milestones(id)`       | persistent   | Milestone tranches for a phased aid program;   |
//! |                        |              | must persist until each tranche is settled.    |
//! | `MilestoneVerifier`    | instance     | Address authorized to verify milestones.       |

use soroban_sdk::{contracttype, Address, Env, Vec};

use shared::storage::{
    instance_get, instance_set, persistent_get, persistent_has, persistent_remove, persistent_set,
};

use crate::types::{AidMilestone, AidRecord};

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Keys used by the aid contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Persistent record for a single aid disbursement.
    Aid(u64),
    /// Monotonically-increasing aid identifier counter (instance).
    AidCounter,
    /// Token address escrowed by this contract (instance).
    Token,
    /// Treasury address (instance).
    Treasury,
    /// Default expiry in seconds (instance).
    DefaultExpiry,
    /// Configuration marker (instance) - set on initialize.
    Initialized,
    /// Append-only list of aid IDs created by a donor (persistent).
    DonorAids(Address),
    /// Append-only list of aid IDs assigned to a recipient (persistent).
    RecipientIndex(Address),
    /// IDs currently eligible for discovery search. Canonical records remain
    /// the source of truth; this is a repairable derived index.
    SearchIndex,
    /// An additional address permitted to discover a particular aid record.
    SearchAccess(u64, Address),
    /// Admin-controlled discovery visibility flag. Missing means visible for
    /// backwards-compatible records.
    SearchHidden(u64),
    /// Correlation ID attached at creation (persistent).
    CorrelationId(u64),
    /// Contract storage schema version (instance).
    StorageSchemaVersion,
    /// Milestone tranches for a phased aid program (persistent).
    Milestones(u64),
    /// Address authorized to verify milestones (instance).
    MilestoneVerifier,
    /// Bucketed page of donor aids (persistent).
    DonorAidsPage(Address, u32),
    /// Total count of aids for a donor (persistent).
    DonorAidCount(Address),
    /// Bucketed page of recipient aids (persistent).
    RecipientAidsPage(Address, u32),
    /// Total count of aids for a recipient (persistent).
    RecipientAidCount(Address),
    /// Bucketed page of search index IDs (persistent).
    SearchIndexPage(u32),
    /// Total count of items in search index (persistent).
    SearchIndexCount,
    /// Authorized inspector address (persistent).
    Inspector(Address),
}

pub const PAGE_SIZE: u32 = 50;

// ---------------------------------------------------------------------------
// Aid record helpers — persistent storage with automatic TTL extension
// ---------------------------------------------------------------------------

/// Read an aid record from persistent storage.
///
/// Returns `None` when the ID does not exist.  Extends the entry TTL on every
/// hit so frequently-accessed records are never evicted while active.
#[inline]
pub fn get_aid(env: &Env, aid_id: u64) -> Option<AidRecord> {
    persistent_get(env, &DataKey::Aid(aid_id))
}

/// Write (or overwrite) an aid record to persistent storage.
///
/// TTL is extended immediately so the entry survives upcoming ledger closures.
#[inline]
pub fn set_aid(env: &Env, aid_id: u64, record: &AidRecord) {
    persistent_set(env, &DataKey::Aid(aid_id), record);
}

/// Returns `true` when an aid record with the given ID exists.
///
/// Does **not** extend TTL — call [`get_aid`] when you need the value.
#[inline]
pub fn has_aid(env: &Env, aid_id: u64) -> bool {
    persistent_has(env, &DataKey::Aid(aid_id))
}

/// Remove an aid record from persistent storage (e.g. after full settlement).
#[inline]
pub fn remove_aid(env: &Env, aid_id: u64) {
    persistent_remove(env, &DataKey::Aid(aid_id));
}

// ---------------------------------------------------------------------------
// Milestone helpers — persistent storage with automatic TTL extension
// ---------------------------------------------------------------------------

/// Read the milestone tranches for an aid program.
///
/// Returns an empty vector when no milestones have been configured, which
/// indicates a single-payout (all-or-nothing) program.
#[inline]
pub fn get_milestones(env: &Env, aid_id: u64) -> Vec<AidMilestone> {
    persistent_get(env, &DataKey::Milestones(aid_id)).unwrap_or_else(|| Vec::new(env))
}

/// Write (or overwrite) the milestone tranches for an aid program.
#[inline]
pub fn set_milestones(env: &Env, aid_id: u64, milestones: &Vec<AidMilestone>) {
    persistent_set(env, &DataKey::Milestones(aid_id), milestones);
}

/// Returns `true` when the aid program has milestone tranches configured.
#[inline]
pub fn has_milestones(env: &Env, aid_id: u64) -> bool {
    persistent_has(env, &DataKey::Milestones(aid_id))
}

/// Remove the milestone tranches for an aid program (e.g. after settlement).
#[inline]
pub fn remove_milestones(env: &Env, aid_id: u64) {
    persistent_remove(env, &DataKey::Milestones(aid_id));
}

// ---------------------------------------------------------------------------
// Aid counter — instance storage
// ---------------------------------------------------------------------------

/// Read the current aid counter, defaulting to 0 if never set.
#[inline]
pub fn get_aid_counter(env: &Env) -> u64 {
    instance_get(env, &DataKey::AidCounter).unwrap_or(0)
}

/// Write the aid counter to instance storage.
#[inline]
pub fn set_aid_counter(env: &Env, counter: u64) {
    instance_set(env, &DataKey::AidCounter, &counter);
}

// ---------------------------------------------------------------------------
// Escrow token — instance storage
// ---------------------------------------------------------------------------

/// Read the configured escrow token, if initialised.
pub fn get_token(env: &Env) -> Option<Address> {
    instance_get(env, &DataKey::Token)
}

/// Store the escrow token address (initialisation only).
pub fn set_token(env: &Env, token: &Address) {
    instance_set(env, &DataKey::Token, token);
}

// ---------------------------------------------------------------------------
// Participant indexes — append-only ID lists for pagination
// ---------------------------------------------------------------------------

pub fn get_donor_aid_count(env: &Env, donor: &Address) -> u32 {
    persistent_get(env, &DataKey::DonorAidCount(donor.clone())).unwrap_or(0)
}

pub fn get_donor_aids_page(env: &Env, donor: &Address, page: u32) -> Vec<u64> {
    persistent_get(env, &DataKey::DonorAidsPage(donor.clone(), page)).unwrap_or_else(|| Vec::new(env))
}

/// Read the full list of aid IDs created by `donor`.
pub fn get_donor_aids(env: &Env, donor: &Address) -> Vec<u64> {
    let count = get_donor_aid_count(env, donor);
    if count == 0 {
        return persistent_get(env, &DataKey::DonorAids(donor.clone())).unwrap_or_else(|| Vec::new(env));
    }
    let total_pages = (count + PAGE_SIZE - 1) / PAGE_SIZE;
    let mut all_ids = Vec::new(env);
    for p in 0..total_pages {
        let page_vec = get_donor_aids_page(env, donor, p);
        for id in page_vec.iter() {
            all_ids.push_back(id);
        }
    }
    all_ids
}

/// Append `aid_id` to `donor`'s index in O(1) writes within a bounded page.
pub fn append_donor_aid(env: &Env, donor: &Address, aid_id: u64) {
    let count = get_donor_aid_count(env, donor);
    let page = count / PAGE_SIZE;
    let mut page_vec = get_donor_aids_page(env, donor, page);
    page_vec.push_back(aid_id);
    persistent_set(env, &DataKey::DonorAidsPage(donor.clone(), page), &page_vec);
    persistent_set(env, &DataKey::DonorAidCount(donor.clone()), &(count + 1));
}

/// Append multiple `aid_ids` to `donor`'s index.
pub fn append_donor_aids(env: &Env, donor: &Address, new_ids: &Vec<u64>) {
    for id in new_ids.iter() {
        append_donor_aid(env, donor, id);
    }
}

pub fn get_recipient_aid_count(env: &Env, recipient: &Address) -> u32 {
    persistent_get(env, &DataKey::RecipientAidCount(recipient.clone())).unwrap_or(0)
}

pub fn get_recipient_aids_page(env: &Env, recipient: &Address, page: u32) -> Vec<u64> {
    persistent_get(env, &DataKey::RecipientAidsPage(recipient.clone(), page)).unwrap_or_else(|| Vec::new(env))
}

/// Read the full list of aid IDs assigned to `recipient`.
pub fn get_recipient_aids(env: &Env, recipient: &Address) -> Vec<u64> {
    let count = get_recipient_aid_count(env, recipient);
    if count == 0 {
        return persistent_get(env, &DataKey::RecipientIndex(recipient.clone())).unwrap_or_else(|| Vec::new(env));
    }
    let total_pages = (count + PAGE_SIZE - 1) / PAGE_SIZE;
    let mut all_ids = Vec::new(env);
    for p in 0..total_pages {
        let page_vec = get_recipient_aids_page(env, recipient, p);
        for id in page_vec.iter() {
            all_ids.push_back(id);
        }
    }
    all_ids
}

/// Append `aid_id` to `recipient`'s index in O(1) writes within a bounded page.
pub fn append_recipient_aid(env: &Env, recipient: &Address, aid_id: u64) {
    let count = get_recipient_aid_count(env, recipient);
    let page = count / PAGE_SIZE;
    let mut page_vec = get_recipient_aids_page(env, recipient, page);
    page_vec.push_back(aid_id);
    persistent_set(env, &DataKey::RecipientAidsPage(recipient.clone(), page), &page_vec);
    persistent_set(env, &DataKey::RecipientAidCount(recipient.clone()), &(count + 1));
}

pub fn get_donor_index(env: &Env, donor: &Address) -> Vec<u64> {
    get_donor_aids(env, donor)
}

pub fn append_donor_index(env: &Env, donor: &Address, aid_id: u64) {
    append_donor_aid(env, donor, aid_id);
}

pub fn get_recipient_index(env: &Env, recipient: &Address) -> Vec<u64> {
    get_recipient_aids(env, recipient)
}

pub fn append_recipient_index(env: &Env, recipient: &Address, aid_id: u64) {
    append_recipient_aid(env, recipient, aid_id);
}

// ---------------------------------------------------------------------------
// Permission-aware discovery index — chunked/bucketed pages
// ---------------------------------------------------------------------------

pub fn get_search_index_count(env: &Env) -> u32 {
    persistent_get(env, &DataKey::SearchIndexCount).unwrap_or(0)
}

pub fn get_search_index_page(env: &Env, page: u32) -> Vec<u64> {
    persistent_get(env, &DataKey::SearchIndexPage(page)).unwrap_or_else(|| Vec::new(env))
}

pub fn get_search_index(env: &Env) -> Vec<u64> {
    let count = get_search_index_count(env);
    if count == 0 {
        return persistent_get(env, &DataKey::SearchIndex).unwrap_or_else(|| Vec::new(env));
    }
    let total_pages = (count + PAGE_SIZE - 1) / PAGE_SIZE;
    let mut all_ids = Vec::new(env);
    for p in 0..total_pages {
        let page_vec = get_search_index_page(env, p);
        for id in page_vec.iter() {
            all_ids.push_back(id);
        }
    }
    all_ids
}

pub fn set_search_index(env: &Env, ids: &Vec<u64>) {
    let total_count = ids.len();
    persistent_set(env, &DataKey::SearchIndexCount, &total_count);
    let total_pages = if total_count == 0 { 0 } else { (total_count + PAGE_SIZE - 1) / PAGE_SIZE };
    for p in 0..total_pages {
        let mut page_vec = Vec::new(env);
        let start = p * PAGE_SIZE;
        let end = ((p + 1) * PAGE_SIZE).min(total_count);
        for i in start..end {
            page_vec.push_back(ids.get(i).unwrap());
        }
        persistent_set(env, &DataKey::SearchIndexPage(p), &page_vec);
    }
}

pub fn is_search_hidden(env: &Env, aid_id: u64) -> bool {
    persistent_get(env, &DataKey::SearchHidden(aid_id)).unwrap_or(false)
}

pub fn set_search_hidden(env: &Env, aid_id: u64, hidden: bool) {
    persistent_set(env, &DataKey::SearchHidden(aid_id), &hidden);
}

pub fn has_search_access(env: &Env, aid_id: u64, viewer: &Address) -> bool {
    persistent_get(env, &DataKey::SearchAccess(aid_id, viewer.clone())).unwrap_or(false)
}

pub fn set_search_access(env: &Env, aid_id: u64, viewer: &Address, granted: bool) {
    persistent_set(
        env,
        &DataKey::SearchAccess(aid_id, viewer.clone()),
        &granted,
    );
}

// ---------------------------------------------------------------------------
// Treasury address — instance storage
// ---------------------------------------------------------------------------

/// Read the configured treasury address, if initialised.
pub fn get_treasury(env: &Env) -> Option<Address> {
    instance_get(env, &DataKey::Treasury)
}

/// Store the treasury address (initialisation only).
pub fn set_treasury(env: &Env, treasury: &Address) {
    instance_set(env, &DataKey::Treasury, treasury);
}

// ---------------------------------------------------------------------------
// Default expiry — instance storage
// ---------------------------------------------------------------------------

/// Read the default expiry in seconds, if set.
pub fn get_default_expiry(env: &Env) -> Option<u64> {
    instance_get(env, &DataKey::DefaultExpiry)
}

/// Store the default expiry in seconds.
pub fn set_default_expiry(env: &Env, expiry_secs: u64) {
    instance_set(env, &DataKey::DefaultExpiry, &expiry_secs);
}

// ---------------------------------------------------------------------------
// Milestone verifier — instance storage
// ---------------------------------------------------------------------------

/// Read the address authorized to verify milestones, if configured.
pub fn get_milestone_verifier(env: &Env) -> Option<Address> {
    instance_get(env, &DataKey::MilestoneVerifier)
}

/// Store the address authorized to verify milestones.
pub fn set_milestone_verifier(env: &Env, verifier: &Address) {
    instance_set(env, &DataKey::MilestoneVerifier, verifier);
}

/// Remove the configured milestone verifier.
pub fn remove_milestone_verifier(env: &Env) {
    persistent_remove(env, &DataKey::MilestoneVerifier);
}

// ---------------------------------------------------------------------------
// Initialization flag — instance storage
// ---------------------------------------------------------------------------

/// Check if the contract has been initialized.
pub fn is_initialized(env: &Env) -> bool {
    instance_get::<DataKey, bool>(env, &DataKey::Initialized).unwrap_or(false)
}

/// Mark the contract as initialized.
pub fn set_initialized(env: &Env) {
    instance_set(env, &DataKey::Initialized, &true);
}

// ---------------------------------------------------------------------------
// Storage schema version — instance storage (Issue #140)
// ---------------------------------------------------------------------------

/// Read the contract storage schema version.
pub fn get_storage_schema_version(env: &Env) -> Option<u32> {
    shared::storage_version::get_storage_schema_version(env)
}

/// Store the explicit contract storage schema version.
pub fn set_storage_schema_version(env: &Env, version: u32) {
    shared::storage_version::set_storage_schema_version(env, version);
}

/// Verify that the storage schema version is currently supported for read paths.
pub fn guard_storage_read(env: &Env) -> Result<u32, shared::Error> {
    shared::storage_version::guard_storage_read(env)
}

pub fn is_inspector(env: &Env, inspector: &Address) -> bool {
    persistent_get(env, &DataKey::Inspector(inspector.clone())).unwrap_or(false)
}

pub fn set_inspector(env: &Env, inspector: &Address, enabled: bool) {
    persistent_set(env, &DataKey::Inspector(inspector.clone()), &enabled);
}
