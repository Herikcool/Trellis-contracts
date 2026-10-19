//! # Aid Contract — Asset Conservation Invariant Test Suite
//!
//! Explicit mathematical invariant proofs proving that humanitarian aid assets
//! cannot be created, lost, or misallocated across any supported protocol transitions:
//!
//! ## Threat Model: Cross-Boundary Trust Decisions
//!
//! See `docs/threat-model-aid-contract.md` for the full scenario-based threat model.
//! The tests below exercise the highest-risk trust boundaries:
//!
//! - Aid disbursement creation (escrow funding)
//! - Recipient aid claiming (settlement)
//! - Expired aid refund (cancellation)
//! - Multi-party concurrent aid disbursements
//! - Failure & rejection paths (reversion invariance)
//! - Fixture mutations (negative proofs detecting impossible balances)
//! - Cross-boundary authorization (admin/treasury/donor/recipient)
//! - Replay & double-spend resistance across status transitions

#![cfg(test)]

extern crate std;
use std::vec::Vec;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Env,
};
use shared::invariants::{
    assert_asset_conservation, assert_reversion_invariance, mutations, verify_conservation,
    AssetConservationSnapshot, InvariantViolation,
};

struct AidTestFixture {
    env: Env,
    admin: Address,
    treasury: Address,
    donor: Address,
    recipient_1: Address,
    recipient_2: Address,
    token_addr: Address,
    token_client: token::Client<'static>,
    asset_client: token::StellarAssetClient<'static>,
    contract_id: Address,
    client: AidContractClient<'static>,
}

fn setup_aid_fixture() -> AidTestFixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let donor = Address::generate(&env);
    let recipient_1 = Address::generate(&env);
    let recipient_2 = Address::generate(&env);

    let token_addr = env.register_stellar_asset_contract(admin.clone());
    let token_client = token::Client::new(&env, &token_addr);
    let asset_client = token::StellarAssetClient::new(&env, &token_addr);

    let contract_id = env.register_contract(None, AidContract);
    let client = AidContractClient::new(&env, &contract_id);

    // Initialize with 86400s (1 day) default expiry
    client.initialize(&admin, &treasury, &token_addr, &86400);

    // Fund donor
    asset_client.mint(&donor, &200_000);

    AidTestFixture {
        env,
        admin,
        treasury,
        donor,
        recipient_1,
        recipient_2,
        token_addr,
        token_client: unsafe { std::mem::transmute(token_client) },
        asset_client: unsafe { std::mem::transmute(asset_client) },
        contract_id,
        client: unsafe { std::mem::transmute(client) },
    }
}

impl AidTestFixture {
    /// Calculate the sum of all pending aid escrow liabilities
    fn active_aid_liabilities(&self, known_aid_ids: &[u64]) -> i128 {
        let mut total: i128 = 0;
        for &id in known_aid_ids {
            if let Some(record) = self.client.get_aid(&id) {
                if record.status == AidStatus::Pending {
                    total += record.amount;
                }
            }
        }
        total
    }

    /// Capture a formal asset conservation snapshot
    fn capture_snapshot(&self, known_aid_ids: &[u64]) -> AssetConservationSnapshot {
        let internal_liabilities = self.active_aid_liabilities(known_aid_ids);
        let participants = [
            (self.donor.clone(), "Donor"),
            (self.recipient_1.clone(), "Recipient1"),
            (self.recipient_2.clone(), "Recipient2"),
            (self.treasury.clone(), "Treasury"),
            (self.admin.clone(), "Admin"),
        ];

        AssetConservationSnapshot::capture(
            &self.env,
            &self.token_client,
            &self.contract_id,
            &participants,
            internal_liabilities,
        )
    }
}

// ---------------------------------------------------------------------------
// 1. Aid Creation (Deposit / Escrow) Invariant Tests
// ---------------------------------------------------------------------------
//
// Trust boundary: donor -> contract escrow.
// Abuse case: donor funds aid but contract does not record matching liability.
// Mitigation: escrow transfer and liability write occur atomically in create_aid.

#[test]
fn test_invariant_create_aid_conserves_assets() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let before = fx.capture_snapshot(&aid_ids);
    let aid_amount: i128 = 50_000;
    let expiry_ledger = fx.env.ledger().sequence() + 100;

    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &aid_amount,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    let after = fx.capture_snapshot(&aid_ids);

    let report = assert_asset_conservation(&before, &after);
    assert_eq!(report.net_system_delta, 0, "No assets created or destroyed");
    assert_eq!(
        report.contract_reserve_delta, aid_amount,
        "Contract reserve strictly increased by aid amount"
    );
    assert_eq!(
        report.internal_liabilities_delta, aid_amount,
        "Pending aid liability matches escrowed amount"
    );

    assert_eq!(
        after.get_balance(&fx.donor).unwrap(),
        before.get_balance(&fx.donor).unwrap() - aid_amount
    );
}

// ---------------------------------------------------------------------------
// 2. Aid Claiming (Settlement) Invariant Tests
// ---------------------------------------------------------------------------
//
// Trust boundary: contract escrow -> recipient.
// Abuse case: non-recipient claims, or recipient double-claims same aid.
// Mitigation: recipient auth + Pending->Claimed status transition is atomic.

#[test]
fn test_invariant_claim_aid_settlement_conserves_assets() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let aid_amount: i128 = 35_000;
    let expiry_ledger = fx.env.ledger().sequence() + 100;

    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &aid_amount,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    let before_claim = fx.capture_snapshot(&aid_ids);

    // Recipient claims the aid
    let claim_res = fx.client.try_claim_aid(&aid_id, &fx.recipient_1);
    assert!(claim_res.is_ok());

    let after_claim = fx.capture_snapshot(&aid_ids);

    let report = assert_asset_conservation(&before_claim, &after_claim);
    assert_eq!(report.net_system_delta, 0);
    assert_eq!(report.contract_reserve_delta, -aid_amount);
    assert_eq!(report.internal_liabilities_delta, -aid_amount);

    assert_eq!(
        after_claim.get_balance(&fx.recipient_1).unwrap(),
        before_claim.get_balance(&fx.recipient_1).unwrap() + aid_amount
    );
}

// ---------------------------------------------------------------------------
// 3. Aid Refund (Cancellation on Expiry) Invariant Tests
// ---------------------------------------------------------------------------
//
// Trust boundary: contract escrow -> donor (refund path).
// Abuse case: premature refund drains escrow before recipient can claim.
// Mitigation: refund requires ledger sequence strictly past expiry_ledger.

#[test]
fn test_invariant_refund_aid_cancellation_conserves_assets() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let aid_amount: i128 = 40_000;
    let expiry_ledger = fx.env.ledger().sequence() + 50;

    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &aid_amount,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    // Fast-forward past expiry
    fx.env.ledger().set_sequence_number(expiry_ledger + 1);

    let before_refund = fx.capture_snapshot(&aid_ids);

    // Donor triggers refund
    let refund_res = fx.client.try_refund_aid(&aid_id, &fx.donor);
    assert!(refund_res.is_ok());

    let after_refund = fx.capture_snapshot(&aid_ids);

    let report = assert_asset_conservation(&before_refund, &after_refund);
    assert_eq!(report.net_system_delta, 0);
    assert_eq!(report.contract_reserve_delta, -aid_amount);
    assert_eq!(report.internal_liabilities_delta, -aid_amount);

    assert_eq!(
        after_refund.get_balance(&fx.donor).unwrap(),
        before_refund.get_balance(&fx.donor).unwrap() + aid_amount
    );
}

// ---------------------------------------------------------------------------
// 4. Concurrent Multi-Disbursement Invariant Tests
// ---------------------------------------------------------------------------
//
// Trust boundary: multiple donors/recipients sharing one contract reserve.
// Abuse case: interleaved claim/refund corrupts per-aid liability accounting.
// Mitigation: each aid_id is independently tracked; reserve == sum(liabilities).

#[test]
fn test_invariant_concurrent_multi_aid_disbursements() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let initial = fx.capture_snapshot(&aid_ids);

    // Create 3 aids
    let aid1 = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &10_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    let aid2 = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_2,
        &20_000,
        &(fx.env.ledger().sequence() + 50),
        &None,
    );
    let aid3 = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &30_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    aid_ids.extend_from_slice(&[aid1, aid2, aid3]);

    let after_all_created = fx.capture_snapshot(&aid_ids);
    assert_asset_conservation(&initial, &after_all_created);
    assert_eq!(after_all_created.contract_token_reserve, 60_000);
    assert_eq!(after_all_created.internal_liabilities, 60_000);

    // Claim aid1
    fx.client.claim_aid(&aid1, &fx.recipient_1);
    let after_claim1 = fx.capture_snapshot(&aid_ids);
    assert_asset_conservation(&after_all_created, &after_claim1);
    assert_eq!(after_claim1.contract_token_reserve, 50_000);

    // Advance time past aid2 expiry and refund aid2
    fx.env.ledger().set_sequence_number(fx.env.ledger().sequence() + 60);
    fx.client.refund_aid(&aid2, &fx.donor);
    let after_refund2 = fx.capture_snapshot(&aid_ids);
    assert_asset_conservation(&after_claim1, &after_refund2);
    assert_eq!(after_refund2.contract_token_reserve, 30_000);

    // Claim aid3
    fx.client.claim_aid(&aid3, &fx.recipient_1);
    let after_claim3 = fx.capture_snapshot(&aid_ids);
    assert_asset_conservation(&after_refund2, &after_claim3);
    assert_eq!(after_claim3.contract_token_reserve, 0);
    assert_eq!(after_claim3.internal_liabilities, 0);

    // Final total system supply matches initial exactly
    assert_eq!(
        after_claim3.total_system_assets(),
        initial.total_system_assets()
    );
}

// ---------------------------------------------------------------------------
// 5. Failure Paths (Reversion Invariance)
// ---------------------------------------------------------------------------
//
// Trust boundary: any caller attempting an unauthorized or invalid transition.
// Abuse case: partial state mutation on rejected call (e.g. reserve drained
//            before auth check fails).
// Mitigation: all validation precedes state mutation; failures revert cleanly.

#[test]
fn test_invariant_aid_failure_paths_preserve_exact_state() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let aid_amount = 25_000;
    let expiry_ledger = fx.env.ledger().sequence() + 40;
    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &aid_amount,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    let baseline = fx.capture_snapshot(&aid_ids);

    // 1. Unauthorized recipient attempts claim
    let err_unauth = fx.client.try_claim_aid(&aid_id, &fx.recipient_2);
    assert!(err_unauth.is_err());
    let after_unauth = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&baseline, &after_unauth);

    // 2. Premature refund before expiry
    let err_early_refund = fx.client.try_refund_aid(&aid_id, &fx.donor);
    assert!(err_early_refund.is_err());
    let after_early = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&baseline, &after_early);

    // 3. Invalid claim on nonexistent aid
    let err_not_found = fx.client.try_claim_aid(&999_999, &fx.recipient_1);
    assert!(err_not_found.is_err());
    let after_not_found = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&baseline, &after_not_found);
}

// ---------------------------------------------------------------------------
// 6. Mutation Testing (Proving Tests Fail on Impossible Balances)
// ---------------------------------------------------------------------------
//
// Trust boundary: the invariant checker itself.
// Abuse case: a buggy invariant that silently accepts corrupted snapshots.
// Mitigation: mutation tests inject impossible states and assert detection.

#[test]
fn test_mutation_impossible_balances_fail_invariant_verification() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &15_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    aid_ids.push(aid_id);

    let before = fx.capture_snapshot(&aid_ids);

    // Mutation 1: Artificial reserve inflation (unbacked token creation)
    let mut corrupted1 = fx.capture_snapshot(&aid_ids);
    mutations::corrupt_inflate_reserve(&mut corrupted1, 5_000);
    match verify_conservation(&before, &corrupted1) {
        Err(InvariantViolation::AssetConservationViolated { net_delta, .. }) => {
            assert_eq!(net_delta, 5_000);
        }
        other => panic!("Expected AssetConservationViolated, got {:?}", other),
    }

    // Mutation 2: Reserve leakage / theft without ledger update
    let mut corrupted2 = fx.capture_snapshot(&aid_ids);
    mutations::corrupt_drain_reserve(&mut corrupted2, 3_000);
    match verify_conservation(&before, &corrupted2) {
        Err(InvariantViolation::AssetConservationViolated { net_delta, .. }) => {
            assert_eq!(net_delta, -3_000);
        }
        other => panic!("Expected AssetConservationViolated, got {:?}", other),
    }

    // Mutation 3: Internal liabilities desynchronization
    let mut corrupted3 = fx.capture_snapshot(&aid_ids);
    mutations::corrupt_internal_liabilities(&mut corrupted3, 2_000);
    match verify_conservation(&before, &corrupted3) {
        Err(InvariantViolation::SolvencyBackingMismatched { discrepancy, .. }) => {
            assert_eq!(discrepancy, -2_000);
        }
        other => panic!("Expected SolvencyBackingMismatched, got {:?}", other),
    }

    // Mutation 4: Impossible negative balance
    let mut corrupted4 = fx.capture_snapshot(&aid_ids);
    mutations::corrupt_negative_balance(&mut corrupted4, &fx.recipient_1);
    match verify_conservation(&before, &corrupted4) {
        Err(InvariantViolation::ImpossibleBalanceDetected { balance, .. }) => {
            assert_eq!(balance, -100);
        }
        other => panic!("Expected ImpossibleBalanceDetected, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// 7. Cross-Boundary Authorization Threat Scenarios
// ---------------------------------------------------------------------------
//
// These tests cover the highest-risk trust boundaries identified in the
// scenario-based threat model (see docs/threat-model-aid-contract.md).

#[test]
fn test_threat_unauthorized_admin_cannot_reinitialize() {
    let fx = setup_aid_fixture();
    let attacker = Address::generate(&fx.env);

    // Abuse case: attacker attempts to re-initialize the contract to hijack
    // admin/treasury/token. Must fail and leave state unchanged.
    let res = fx.client.try_initialize(
        &attacker,
        &attacker,
        &fx.token_addr,
        &86400,
    );
    assert!(res.is_err(), "re-initialize must be rejected");

    // Verify original admin still controls the contract by creating an aid.
    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &1_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    assert!(fx.client.get_aid(&aid_id).is_some());
}

#[test]
fn test_threat_double_claim_rejected_and_conserves_assets() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &12_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    aid_ids.push(aid_id);

    // First claim succeeds.
    fx.client.claim_aid(&aid_id, &fx.recipient_1);
    let after_first = fx.capture_snapshot(&aid_ids);

    // Abuse case: replay the same claim. Must revert with no state change.
    let replay = fx.client.try_claim_aid(&aid_id, &fx.recipient_1);
    assert!(replay.is_err(), "double claim must be rejected");

    let after_replay = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&after_first, &after_replay);
    assert_eq!(after_replay.contract_token_reserve, 0);
    assert_eq!(after_replay.internal_liabilities, 0);
}

#[test]
fn test_threat_claim_after_refund_rejected() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let expiry_ledger = fx.env.ledger().sequence() + 10;
    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &8_000,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    // Advance past expiry and refund to donor.
    fx.env.ledger().set_sequence_number(expiry_ledger + 1);
    fx.client.refund_aid(&aid_id, &fx.donor);
    let after_refund = fx.capture_snapshot(&aid_ids);

    // Abuse case: recipient races the refund and tries to claim afterwards.
    let late_claim = fx.client.try_claim_aid(&aid_id, &fx.recipient_1);
    assert!(late_claim.is_err(), "claim after refund must be rejected");

    let after_late = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&after_refund, &after_late);
}

#[test]
fn test_threat_refund_after_claim_rejected() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let expiry_ledger = fx.env.ledger().sequence() + 10;
    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &9_000,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    // Recipient claims before expiry.
    fx.client.claim_aid(&aid_id, &fx.recipient_1);
    let after_claim = fx.capture_snapshot(&aid_ids);

    // Abuse case: donor attempts to refund an already-claimed aid.
    fx.env.ledger().set_sequence_number(expiry_ledger + 1);
    let late_refund = fx.client.try_refund_aid(&aid_id, &fx.donor);
    assert!(late_refund.is_err(), "refund after claim must be rejected");

    let after_late = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&after_claim, &after_late);
}

#[test]
fn test_threat_unauthorized_refund_by_non_donor_rejected() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let expiry_ledger = fx.env.ledger().sequence() + 10;
    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &7_500,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    let baseline = fx.capture_snapshot(&aid_ids);

    // Advance past expiry so the only remaining check is authorization.
    fx.env.ledger().set_sequence_number(expiry_ledger + 1);

    // Abuse case: a third party (recipient_2) attempts to trigger refund.
    let res = fx.client.try_refund_aid(&aid_id, &fx.recipient_2);
    assert!(res.is_err(), "non-donor refund must be rejected");

    let after = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&baseline, &after);
    assert_eq!(after.contract_token_reserve, 7_500);
    assert_eq!(after.internal_liabilities, 7_500);
}

#[test]
fn test_threat_zero_and_negative_amount_rejected() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();
    let baseline = fx.capture_snapshot(&aid_ids);
    let expiry_ledger = fx.env.ledger().sequence() + 100;

    // Abuse case: zero-amount aid could bypass liability checks.
    let zero = fx.client.try_create_aid(
        &fx.donor,
        &fx.recipient_1,
        &0,
        &expiry_ledger,
        &None,
    );
    assert!(zero.is_err(), "zero-amount aid must be rejected");

    // Abuse case: negative amount could invert escrow direction.
    let negative = fx.client.try_create_aid(
        &fx.donor,
        &fx.recipient_1,
        &-1_000,
        &expiry_ledger,
        &None,
    );
    assert!(negative.is_err(), "negative-amount aid must be rejected");

    let after = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&baseline, &after);
}

#[test]
fn test_threat_expired_aid_cannot_be_claimed() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    let expiry_ledger = fx.env.ledger().sequence() + 5;
    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &6_000,
        &expiry_ledger,
        &None,
    );
    aid_ids.push(aid_id);

    let baseline = fx.capture_snapshot(&aid_ids);

    // Advance strictly past expiry.
    fx.env.ledger().set_sequence_number(expiry_ledger + 1);

    // Abuse case: recipient attempts to claim after expiry window.
    let res = fx.client.try_claim_aid(&aid_id, &fx.recipient_1);
    assert!(res.is_err(), "expired aid claim must be rejected");

    let after = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&baseline, &after);
}

// ---------------------------------------------------------------------------
// 8. Operational Edge Cases & Recovery Invariants
// ---------------------------------------------------------------------------
//
// These tests document failure modes and rollback/recovery expectations for
// operational incidents. See docs/threat-model-aid-contract.md for the
// corresponding runbook.

#[test]
fn test_recovery_partial_batch_failure_leaves_consistent_state() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    // Successful aid.
    let good = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &5_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    aid_ids.push(good);

    let after_good = fx.capture_snapshot(&aid_ids);

    // Failing aid (zero amount) must not disturb the successful one.
    let bad = fx.client.try_create_aid(
        &fx.donor,
        &fx.recipient_2,
        &0,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    assert!(bad.is_err());

    let after_bad = fx.capture_snapshot(&aid_ids);
    assert_reversion_invariance(&after_good, &after_bad);
    assert_eq!(after_bad.contract_token_reserve, 5_000);
    assert_eq!(after_bad.internal_liabilities, 5_000);
}

#[test]
fn test_recovery_after_full_drain_contract_accepts_new_aid() {
    let fx = setup_aid_fixture();
    let mut aid_ids = Vec::new();

    // Create and fully settle one aid.
    let aid_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_1,
        &4_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    aid_ids.push(aid_id);
    fx.client.claim_aid(&aid_id, &fx.recipient_1);

    let drained = fx.capture_snapshot(&aid_ids);
    assert_eq!(drained.contract_token_reserve, 0);
    assert_eq!(drained.internal_liabilities, 0);

    // Recovery: contract must accept new aid after full drain.
    let new_id = fx.client.create_aid(
        &fx.donor,
        &fx.recipient_2,
        &3_000,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );
    aid_ids.push(new_id);

    let after = fx.capture_snapshot(&aid_ids);
    assert_asset_conservation(&drained, &after);
    assert_eq!(after.contract_token_reserve, 3_000);
    assert_eq!(after.internal_liabilities, 3_000);
}
