//! # Aid Contract — Asset Conservation Invariant Test Suite
//!
//! Explicit mathematical invariant proofs proving that humanitarian aid assets
//! cannot be created, lost, or misallocated across any supported protocol transitions:
//! - Aid disbursement creation (escrow funding)
//! - Recipient aid claiming (settlement)
//! - Expired aid refund (cancellation)
//! - Multi-party concurrent aid disbursements
//! - Failure & rejection paths (reversion invariance)
//! - Fixture mutations (negative proofs detecting impossible balances)

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
