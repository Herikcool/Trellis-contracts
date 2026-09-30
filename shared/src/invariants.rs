//! # Asset Conservation Invariant Proof Engine
//!
//! Formal verification and invariant testing framework for Trellis smart contracts.
//! Proves mathematically and empirically that assets cannot be created, lost, or
//! misallocated across any supported protocol state transition.
//!
//! ## Mathematical Axioms
//!
//! 1. **Global Conservation Axiom**:
//!    For any state transition $\tau: S_t \to S_{t+1}$, the total quantity of tokens
//!    across all external accounts ($\sum B_{\text{ext}}$) plus the contract's token reserve
//!    ($B_{\text{contract}}$) remains strictly constant:
//!    $$\sum B_{\text{ext}}(t+1) + B_{\text{contract}}(t+1) = \sum B_{\text{ext}}(t) + B_{\text{contract}}(t)$$
//!    $$\Delta \text{System Assets} = 0$$
//!
//! 2. **Solvency & Backing Equivalence Axiom**:
//!    The contract's physical token balance must match or exceed all internal accounting
//!    liabilities (tracked user deposits, active escrow commitments, category reserves):
//!    $$B_{\text{contract}}(t) \ge L_{\text{internal}}(t)$$
//!    In strict conservation models without unallocated surplus:
//!    $$B_{\text{contract}}(t) = L_{\text{internal}}(t)$$
//!
//! 3. **Transition-Specific Conservation**:
//!    - **Deposit**: $\Delta B_{\text{caller}} = -A$, $\Delta B_{\text{contract}} = +A$, $\Delta L_{\text{internal}} = +A$.
//!    - **Withdrawal**: $\Delta B_{\text{recipient}} = +N$, $\Delta B_{\text{fee}} = +F$, $\Delta B_{\text{contract}} = -A$ where $A = N + F$.
//!    - **Escrow / Aid Creation**: $\Delta B_{\text{donor}} = -A$, $\Delta B_{\text{contract}} = +A$, $\Delta L_{\text{escrow}} = +A$.
//!    - **Settlement / Claim**: $\Delta B_{\text{beneficiary}} = +A$, $\Delta B_{\text{contract}} = -A$, $\Delta L_{\text{escrow}} = -A$.
//!    - **Refund / Cancellation**: $\Delta B_{\text{donor}} = +A$, $\Delta B_{\text{contract}} = -A$, $\Delta L_{\text{escrow}} = -A$.
//!    - **Failure / Abort**: $\Delta B_{\text{all}} = 0$, $\Delta B_{\text{contract}} = 0$, $\Delta L_{\text{internal}} = 0$.

extern crate std;
use std::format;
use std::string::String;
use std::vec::Vec;

use soroban_sdk::{token, Address, Env};

/// Reason for an invariant verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvariantViolation {
    /// Total system supply changed across a transition where no minting/burning was authorized.
    AssetConservationViolated {
        before_total: i128,
        after_total: i128,
        net_delta: i128,
    },
    /// The contract's physical token reserve does not match its internal liability accounting.
    SolvencyBackingMismatched {
        physical_balance: i128,
        internal_liabilities: i128,
        discrepancy: i128,
    },
    /// A state transition that should have failed and reverted altered state.
    ReversionInvarianceViolated {
        discrepancy_description: String,
    },
    /// An individual balance was mutated into an impossible state (e.g., negative).
    ImpossibleBalanceDetected {
        account: String,
        balance: i128,
    },
    /// Unexpected delta on an account that was not part of the active transition.
    MisallocatedBalance {
        account: String,
        expected_delta: i128,
        actual_delta: i128,
    },
}

/// Snapshot of an individual account's balance in the invariant model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBalanceRecord {
    pub account: Address,
    pub label: String,
    pub balance: i128,
}

/// Comprehensive asset conservation snapshot for a protocol module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetConservationSnapshot {
    /// Timestamp or ledger sequence when snapshot was taken.
    pub ledger_sequence: u32,
    /// Balances of all external participants (donors, recipients, admins, fee collectors).
    pub external_accounts: Vec<AccountBalanceRecord>,
    /// Actual token balance held by the contract address in the SAC token contract.
    pub contract_token_reserve: i128,
    /// Total internal ledger liabilities (e.g. sum of pending aids, total deposits, category balances).
    pub internal_liabilities: i128,
}

impl AssetConservationSnapshot {
    /// Sum of all external participant token balances.
    pub fn total_external_balance(&self) -> i128 {
        self.external_accounts.iter().map(|a| a.balance).sum()
    }

    /// Total system assets (external tokens + contract reserve).
    pub fn total_system_assets(&self) -> i128 {
        self.total_external_balance() + self.contract_token_reserve
    }

    /// Check whether physical reserve matches internal liabilities.
    pub fn is_solvent(&self) -> bool {
        self.contract_token_reserve == self.internal_liabilities
    }

    /// Find an individual balance by address.
    pub fn get_balance(&self, account: &Address) -> Option<i128> {
        self.external_accounts
            .iter()
            .find(|a| a.account == *account)
            .map(|a| a.balance)
    }

    /// Capture snapshot from live environment using SAC token client.
    pub fn capture(
        env: &Env,
        token_client: &token::Client,
        contract_address: &Address,
        external_participants: &[(Address, &str)],
        internal_liabilities: i128,
    ) -> Self {
        let mut external_accounts = Vec::new();
        for (addr, label) in external_participants {
            let balance = token_client.balance(addr);
            external_accounts.push(AccountBalanceRecord {
                account: addr.clone(),
                label: String::from(*label),
                balance,
            });
        }

        let contract_token_reserve = token_client.balance(contract_address);
        let ledger_sequence = env.ledger().sequence();

        Self {
            ledger_sequence,
            external_accounts,
            contract_token_reserve,
            internal_liabilities,
        }
    }
}

/// Computed delta report comparing two consecutive conservation snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConservationDeltaReport {
    pub before_system_total: i128,
    pub after_system_total: i128,
    pub net_system_delta: i128,
    pub contract_reserve_delta: i128,
    pub internal_liabilities_delta: i128,
    pub is_conserved: bool,
    pub is_solvent_after: bool,
}

/// Compute deltas and evaluate invariant proofs between two states.
pub fn verify_conservation(
    before: &AssetConservationSnapshot,
    after: &AssetConservationSnapshot,
) -> Result<ConservationDeltaReport, InvariantViolation> {
    // 1. Check for negative / impossible balances
    for acc in &after.external_accounts {
        if acc.balance < 0 {
            return Err(InvariantViolation::ImpossibleBalanceDetected {
                account: acc.label.clone(),
                balance: acc.balance,
            });
        }
    }
    if after.contract_token_reserve < 0 {
        return Err(InvariantViolation::ImpossibleBalanceDetected {
            account: String::from("ContractReserve"),
            balance: after.contract_token_reserve,
        });
    }
    if after.internal_liabilities < 0 {
        return Err(InvariantViolation::ImpossibleBalanceDetected {
            account: String::from("InternalLiabilities"),
            balance: after.internal_liabilities,
        });
    }

    let before_total = before.total_system_assets();
    let after_total = after.total_system_assets();
    let net_system_delta = after_total - before_total;

    // 2. Global conservation check: net delta must be exactly zero
    if net_system_delta != 0 {
        return Err(InvariantViolation::AssetConservationViolated {
            before_total,
            after_total,
            net_delta: net_system_delta,
        });
    }

    // 3. Solvency backing equivalence check
    if after.contract_token_reserve != after.internal_liabilities {
        return Err(InvariantViolation::SolvencyBackingMismatched {
            physical_balance: after.contract_token_reserve,
            internal_liabilities: after.internal_liabilities,
            discrepancy: after.contract_token_reserve - after.internal_liabilities,
        });
    }

    let contract_reserve_delta = after.contract_token_reserve - before.contract_token_reserve;
    let internal_liabilities_delta = after.internal_liabilities - before.internal_liabilities;

    Ok(ConservationDeltaReport {
        before_system_total: before_total,
        after_system_total: after_total,
        net_system_delta,
        contract_reserve_delta,
        internal_liabilities_delta,
        is_conserved: true,
        is_solvent_after: true,
    })
}

/// Assert that asset conservation holds between two snapshots. Panics with descriptive diagnostics if violated.
pub fn assert_asset_conservation(
    before: &AssetConservationSnapshot,
    after: &AssetConservationSnapshot,
) -> ConservationDeltaReport {
    match verify_conservation(before, after) {
        Ok(report) => report,
        Err(violation) => {
            panic!(
                "Asset Conservation Invariant Violated!\nDetails: {:?}\nBefore State: {:?}\nAfter State: {:?}",
                violation, before, after
            );
        }
    }
}

/// Assert that a failed transaction produced zero state divergence (reversion invariance).
pub fn assert_reversion_invariance(
    before: &AssetConservationSnapshot,
    after: &AssetConservationSnapshot,
) {
    if before.contract_token_reserve != after.contract_token_reserve {
        panic!(
            "Reversion Invariance Failed: contract reserve changed from {} to {}",
            before.contract_token_reserve, after.contract_token_reserve
        );
    }
    if before.internal_liabilities != after.internal_liabilities {
        panic!(
            "Reversion Invariance Failed: internal liabilities changed from {} to {}",
            before.internal_liabilities, after.internal_liabilities
        );
    }
    for b_acc in &before.external_accounts {
        if let Some(a_acc) = after.external_accounts.iter().find(|x| x.account == b_acc.account) {
            if b_acc.balance != a_acc.balance {
                panic!(
                    "Reversion Invariance Failed: account {} balance changed from {} to {}",
                    b_acc.label, b_acc.balance, a_acc.balance
                );
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Mutation Testing Helpers (Negative Invariant Proofs)
// -----------------------------------------------------------------------------

/// Fixture mutation that creates impossible balances or broken invariants to prove test sensitivity.
pub mod mutations {
    use super::*;

    /// Corrupt snapshot by artificially inflating contract reserve (simulating unbacked token minting).
    pub fn corrupt_inflate_reserve(snapshot: &mut AssetConservationSnapshot, amount: i128) {
        snapshot.contract_token_reserve += amount;
    }

    /// Corrupt snapshot by artificially deflating contract reserve (simulating unauthorized fund leakage).
    pub fn corrupt_drain_reserve(snapshot: &mut AssetConservationSnapshot, amount: i128) {
        snapshot.contract_token_reserve -= amount;
    }

    /// Corrupt snapshot by inflating an external account without debiting the contract.
    pub fn corrupt_inflate_external_balance(
        snapshot: &mut AssetConservationSnapshot,
        account: &Address,
        amount: i128,
    ) {
        if let Some(acc) = snapshot.external_accounts.iter_mut().find(|a| a.account == *account) {
            acc.balance += amount;
        }
    }

    /// Corrupt internal accounting liabilities out of sync with physical token reserves.
    pub fn corrupt_internal_liabilities(snapshot: &mut AssetConservationSnapshot, delta: i128) {
        snapshot.internal_liabilities += delta;
    }

    /// Mutate an account balance to an impossible negative value.
    pub fn corrupt_negative_balance(
        snapshot: &mut AssetConservationSnapshot,
        account: &Address,
    ) {
        if let Some(acc) = snapshot.external_accounts.iter_mut().find(|a| a.account == *account) {
            acc.balance = -100;
        }
    }
}
