use soroban_sdk::{contracttype, Address, Symbol, Vec};

/// Contract configuration stored in instance storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub admin: Address,
    pub treasury: Address,
    pub token: Address,
    pub default_expiry_secs: u64,
}

/// A single allocation within a batch aid disbursement request.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchAidAllocation {
    pub recipient: Address,
    pub amount: i128,
    pub purpose: Symbol,
    pub expiry_ledger: u32,
}

/// Lifecycle state of an aid record.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AidStatus {
    /// Escrowed, waiting for a valid claim.
    Pending,
    /// Claimed by the recipient; funds transferred out.
    Settled,
    /// Expired and refunded back to the donor (never claimed).
    Refunded,
    /// Some (but not all) milestone tranches have been claimed.
    PartiallyClaimed,
}

/// A single milestone tranche within a phased aid program.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AidMilestone {
    /// Short human-readable description of the milestone.
    pub description: Symbol,
    /// Amount escrowed for this tranche.
    pub amount: i128,
    /// Whether an authorized inspector has verified completion.
    pub verified: bool,
    /// Whether the recipient has claimed this tranche.
    pub claimed: bool,
}

/// A single aid disbursement record, escrowed inside this contract until
/// claimed by the recipient (or refunded to the donor after expiry).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AidRecord {
    pub id: u64,
    pub donor: Address,
    pub recipient: Address,
    pub token: Address,
    pub amount: i128,
    /// Ledger sequence after which the aid can no longer be claimed.
    pub expiry_ledger: u32,
    pub status: AidStatus,
    /// Number of milestone tranches. Zero means a single-payout program.
    pub milestone_count: u32,
}

/// One page of results from a paginated aid query.
///
/// Pass `next_cursor` back as the `cursor` argument to fetch the following
/// page; `None` means the result set is exhausted.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AidPage {
    pub records: Vec<AidRecord>,
    pub next_cursor: Option<u32>,
}

/// Stable keyset-paginated response container for aid records.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AidPageResponse {
    pub records: Vec<AidRecord>,
    pub next_cursor: Option<u64>,
    pub has_more: bool,
    pub scanned_count: u32,
}

/// Outcome of rebuilding the discovery index from canonical aid storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchIndexRepairReport {
    /// Number of pending, visible aid records placed in the rebuilt index.
    pub indexed: u32,
    /// Entries missing from the previous index.
    pub added: u32,
    /// Stale entries removed from the index.
    pub removed: u32,
}
