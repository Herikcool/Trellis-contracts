//! # Payments Contract — Asset Conservation Invariant Test Suite
//!
//! Explicit mathematical invariant proofs proving that assets cannot be created,
//! lost, or misallocated across any supported payment state transitions:
//! - Deposits
//! - Withdrawals (full & partial with fee deduction)
//! - Escrow creation
//! - Escrow settlement (release to beneficiary)
//! - Escrow cancellation (refund to depositor)
//! - Batch payouts
//! - Failure & reversion paths
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
    AccountBalanceRecord, AssetConservationSnapshot, InvariantViolation,
};

struct TestFixture {
    env: Env,
    admin: Address,
    fee_recipient: Address,
    user_a: Address,
    user_b: Address,
    token_addr: Address,
    token_client: token::Client<'static>,
    asset_client: token::StellarAssetClient<'static>,
    contract_id: Address,
    client: ExamplePaymentsContractClient<'static>,
}

fn setup_fixture() -> TestFixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let fee_recipient = Address::generate(&env);
    let user_a = Address::generate(&env);
    let user_b = Address::generate(&env);

    let token_addr = env.register_stellar_asset_contract(admin.clone());
    let token_client = token::Client::new(&env, &token_addr);
    let asset_client = token::StellarAssetClient::new(&env, &token_addr);

    let contract_id = env.register_contract(None, ExamplePaymentsContract);
    let client = ExamplePaymentsContractClient::new(&env, &contract_id);

    // Initialize with 2.5% fee (250 bps)
    client.initialize(&admin, &token_addr, &250, &fee_recipient);

    // Initial mint to users
    asset_client.mint(&user_a, &100_000);
    asset_client.mint(&user_b, &50_000);

    TestFixture {
        env,
        admin,
        fee_recipient,
        user_a,
        user_b,
        token_addr,
        token_client: unsafe { std::mem::transmute(token_client) },
        asset_client: unsafe { std::mem::transmute(asset_client) },
        contract_id,
        client: unsafe { std::mem::transmute(client) },
    }
}

impl TestFixture {
    /// Calculate the sum of active escrow liabilities currently in contract storage
    fn active_escrow_liabilities(&self, known_escrow_ids: &[u64]) -> i128 {
        let mut total: i128 = 0;
        for &id in known_escrow_ids {
            if let Some(escrow) = self.client.get_escrow_entry(&id) {
                if escrow.state == shared::payments::EscrowState::Active {
                    total += escrow.amount;
                }
            }
        }
        total
    }

    /// Capture a formal asset conservation snapshot
    fn capture_snapshot(&self, known_escrow_ids: &[u64]) -> AssetConservationSnapshot {
        let total_deposits = self.client.total_deposits(&self.token_addr);
        let active_escrows = self.active_escrow_liabilities(known_escrow_ids);
        let internal_liabilities = total_deposits + active_escrows;

        let participants = [
            (self.user_a.clone(), "UserA"),
            (self.user_b.clone(), "UserB"),
            (self.fee_recipient.clone(), "FeeRecipient"),
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
// 1. Deposit Path Invariant Tests
// ---------------------------------------------------------------------------

#[test]
fn test_invariant_deposit_conserves_total_assets() {
    let fx = setup_fixture();
    let escrows = [];

    let before = fx.capture_snapshot(&escrows);
    let deposit_amount: i128 = 25_000;

    fx.client
        .deposit(&fx.user_a, &fx.token_addr, &deposit_amount);

    let after = fx.capture_snapshot(&escrows);

    let report = assert_asset_conservation(&before, &after);
    assert_eq!(report.net_system_delta, 0, "No assets created or destroyed");
    assert_eq!(
        report.contract_reserve_delta, deposit_amount,
        "Contract reserve strictly increased by deposited amount"
    );
    assert_eq!(
        report.internal_liabilities_delta, deposit_amount,
        "Internal liabilities strictly match deposit"
    );

    // Verify individual account delta
    assert_eq!(
        after.get_balance(&fx.user_a).unwrap(),
        before.get_balance(&fx.user_a).unwrap() - deposit_amount
    );
}

// ---------------------------------------------------------------------------
// 2. Withdrawal Path Invariant Tests (Full & Partial with Fee Split)
// ---------------------------------------------------------------------------

#[test]
fn test_invariant_full_withdrawal_with_fee_conserves_assets() {
    let fx = setup_fixture();
    let escrows = [];

    // Pre-condition: deposit 20,000
    fx.client.deposit(&fx.user_a, &fx.token_addr, &20_000);

    let before = fx.capture_snapshot(&escrows);

    // Action: full withdrawal
    let net = fx.client.withdraw(&fx.user_a, &fx.token_addr);

    let after = fx.capture_snapshot(&escrows);

    let report = assert_asset_conservation(&before, &after);
    assert_eq!(report.net_system_delta, 0);
    assert_eq!(report.contract_reserve_delta, -20_000);
    assert_eq!(report.internal_liabilities_delta, -20_000);

    // 250 bps on 20,000 = 500 fee, 19,500 net
    assert_eq!(net, 19_500);
    assert_eq!(
        after.get_balance(&fx.user_a).unwrap(),
        before.get_balance(&fx.user_a).unwrap() + 19_500
    );
    assert_eq!(
        after.get_balance(&fx.fee_recipient).unwrap(),
        before.get_balance(&fx.fee_recipient).unwrap() + 500
    );
}

#[test]
fn test_invariant_partial_withdrawal_conserves_assets() {
    let fx = setup_fixture();
    let escrows = [];

    fx.client.deposit(&fx.user_a, &fx.token_addr, &30_000);

    let before = fx.capture_snapshot(&escrows);
    let withdraw_amount: i128 = 10_000;

    let net = fx
        .client
        .withdraw_amount(&fx.user_a, &fx.token_addr, &withdraw_amount);

    let after = fx.capture_snapshot(&escrows);

    let report = assert_asset_conservation(&before, &after);
    assert_eq!(report.net_system_delta, 0);
    assert_eq!(report.contract_reserve_delta, -withdraw_amount);
    assert_eq!(report.internal_liabilities_delta, -withdraw_amount);

    // 250 bps on 10,000 = 250 fee, 9,750 net
    assert_eq!(net, 9_750);
    assert_eq!(
        after.get_balance(&fx.user_a).unwrap(),
        before.get_balance(&fx.user_a).unwrap() + 9_750
    );
    assert_eq!(
        after.get_balance(&fx.fee_recipient).unwrap(),
        before.get_balance(&fx.fee_recipient).unwrap() + 250
    );
}

// ---------------------------------------------------------------------------
// 3. Escrow Settlement and Cancellation Invariant Tests
// ---------------------------------------------------------------------------

#[test]
fn test_invariant_escrow_settlement_lifecycle_conserves_assets() {
    let fx = setup_fixture();
    let mut escrows = Vec::new();

    // Step 1: Create Escrow (Deposit into escrow)
    let before_create = fx.capture_snapshot(&escrows);
    let escrow_amount: i128 = 12_000;
    let expiry_ledger = fx.env.ledger().sequence() + 100;

    let escrow_id = fx.client.create_escrow_entry(
        &fx.user_a,
        &fx.user_b,
        &fx.token_addr,
        &escrow_amount,
        &expiry_ledger,
    );
    escrows.push(escrow_id);

    let after_create = fx.capture_snapshot(&escrows);

    let create_report = assert_asset_conservation(&before_create, &after_create);
    assert_eq!(create_report.net_system_delta, 0);
    assert_eq!(create_report.contract_reserve_delta, escrow_amount);
    assert_eq!(create_report.internal_liabilities_delta, escrow_amount);

    // Step 2: Settle Escrow (Release to Beneficiary)
    let before_release = after_create;

    fx.client.release_escrow_entry(&fx.admin, &escrow_id);

    let after_release = fx.capture_snapshot(&escrows);

    let release_report = assert_asset_conservation(&before_release, &after_release);
    assert_eq!(release_report.net_system_delta, 0);
    assert_eq!(release_report.contract_reserve_delta, -escrow_amount);
    assert_eq!(release_report.internal_liabilities_delta, -escrow_amount);

    // Beneficiary received the exact escrowed amount
    assert_eq!(
        after_release.get_balance(&fx.user_b).unwrap(),
        before_release.get_balance(&fx.user_b).unwrap() + escrow_amount
    );
}

#[test]
fn test_invariant_escrow_cancellation_refund_conserves_assets() {
    let fx = setup_fixture();
    let mut escrows = Vec::new();

    let before_create = fx.capture_snapshot(&escrows);
    let escrow_amount: i128 = 15_000;
    let expiry_ledger = fx.env.ledger().sequence() + 50;

    let escrow_id = fx.client.create_escrow_entry(
        &fx.user_a,
        &fx.user_b,
        &fx.token_addr,
        &escrow_amount,
        &expiry_ledger,
    );
    escrows.push(escrow_id);

    let after_create = fx.capture_snapshot(&escrows);
    assert_asset_conservation(&before_create, &after_create);

    // Cancel / Refund escrow back to depositor
    let before_refund = after_create;

    fx.client.refund_escrow_entry(&fx.admin, &escrow_id);

    let after_refund = fx.capture_snapshot(&escrows);

    let refund_report = assert_asset_conservation(&before_refund, &after_refund);
    assert_eq!(refund_report.net_system_delta, 0);
    assert_eq!(refund_report.contract_reserve_delta, -escrow_amount);
    assert_eq!(refund_report.internal_liabilities_delta, -escrow_amount);

    // Depositor refunded fully
    assert_eq!(
        after_refund.get_balance(&fx.user_a).unwrap(),
        before_create.get_balance(&fx.user_a).unwrap()
    );
}

// ---------------------------------------------------------------------------
// 4. Batch Payout Invariant Tests
// ---------------------------------------------------------------------------

#[test]
fn test_invariant_batch_payout_conserves_assets() {
    let fx = setup_fixture();
    let escrows = [];

    // Fund the contract directly with 10,000 for batch disbursement
    fx.asset_client.mint(&fx.contract_id, &10_000);

    let mut recipients = soroban_sdk::Vec::new(&fx.env);
    recipients.push_back((fx.user_a.clone(), 4_000));
    recipients.push_back((fx.user_b.clone(), 6_000));

    // Note: unbacked minting to contract is a setup operation; capture snapshot after mint
    let before = fx.capture_snapshot(&escrows);

    let batch_result = fx
        .client
        .batch_payout(&fx.admin, &fx.token_addr, &recipients);
    assert_eq!(batch_result.succeeded, 2);

    let after = fx.capture_snapshot(&escrows);

    // For batch payout from contract balance:
    // Contract reserve decreased by 10,000, external accounts increased by 10,000
    let delta = after.total_system_assets() - before.total_system_assets();
    assert_eq!(delta, 0, "Batch payout strictly conserved total assets");
    assert_eq!(
        after.contract_token_reserve,
        before.contract_token_reserve - 10_000
    );
    assert_eq!(
        after.get_balance(&fx.user_a).unwrap(),
        before.get_balance(&fx.user_a).unwrap() + 4_000
    );
    assert_eq!(
        after.get_balance(&fx.user_b).unwrap(),
        before.get_balance(&fx.user_b).unwrap() + 6_000
    );
}

// ---------------------------------------------------------------------------
// 5. Failure Paths Invariant Tests (Reversion Invariance)
// ---------------------------------------------------------------------------

#[test]
fn test_invariant_failure_paths_preserve_exact_state() {
    let fx = setup_fixture();
    let escrows = [];

    fx.client.deposit(&fx.user_a, &fx.token_addr, &5_000);

    let before = fx.capture_snapshot(&escrows);

    // 1. Insufficient balance withdrawal attempt
    let err_withdraw = fx
        .client
        .try_withdraw_amount(&fx.user_a, &fx.token_addr, &999_999);
    assert!(err_withdraw.is_err());
    let after_failed_withdraw = fx.capture_snapshot(&escrows);
    assert_reversion_invariance(&before, &after_failed_withdraw);

    // 2. Invalid zero-amount deposit attempt
    let err_deposit = fx.client.try_deposit(&fx.user_a, &fx.token_addr, &0);
    assert!(err_deposit.is_err());
    let after_failed_deposit = fx.capture_snapshot(&escrows);
    assert_reversion_invariance(&before, &after_failed_deposit);

    // 3. Unauthorized release of nonexistent escrow
    let err_escrow = fx.client.try_release_escrow_entry(&fx.admin, &999_999);
    assert!(err_escrow.is_err());
    let after_failed_escrow = fx.capture_snapshot(&escrows);
    assert_reversion_invariance(&before, &after_failed_escrow);
}

// ---------------------------------------------------------------------------
// 6. Mutation Invariant Tests (Proving Tests Fail on Impossible Balances)
// ---------------------------------------------------------------------------

#[test]
fn test_mutation_impossible_balances_fail_invariant_verification() {
    let fx = setup_fixture();
    let escrows = [];

    fx.client.deposit(&fx.user_a, &fx.token_addr, &10_000);

    let before = fx.capture_snapshot(&escrows);
    let mut corrupted = fx.capture_snapshot(&escrows);

    // Mutation 1: Artificial reserve inflation (impossible money creation)
    mutations::corrupt_inflate_reserve(&mut corrupted, 5_000);
    match verify_conservation(&before, &corrupted) {
        Err(InvariantViolation::AssetConservationViolated { net_delta, .. }) => {
            assert_eq!(net_delta, 5_000, "Caught inflation of 5000 tokens");
        }
        other => panic!("Expected AssetConservationViolated, got {:?}", other),
    }

    // Reset and Mutation 2: Asset leakage / drain without debit
    let mut corrupted2 = fx.capture_snapshot(&escrows);
    mutations::corrupt_drain_reserve(&mut corrupted2, 2_000);
    match verify_conservation(&before, &corrupted2) {
        Err(InvariantViolation::AssetConservationViolated { net_delta, .. }) => {
            assert_eq!(net_delta, -2_000, "Caught asset drain of 2000 tokens");
        }
        other => panic!("Expected AssetConservationViolated, got {:?}", other),
    }

    // Mutation 3: Internal liability desync (backing insolvency)
    let mut corrupted3 = fx.capture_snapshot(&escrows);
    mutations::corrupt_internal_liabilities(&mut corrupted3, 3_000);
    match verify_conservation(&before, &corrupted3) {
        Err(InvariantViolation::SolvencyBackingMismatched { discrepancy, .. }) => {
            assert_eq!(discrepancy, -3_000, "Caught internal liability mismatch");
        }
        other => panic!("Expected SolvencyBackingMismatched, got {:?}", other),
    }

    // Mutation 4: Negative balance corruption
    let mut corrupted4 = fx.capture_snapshot(&escrows);
    mutations::corrupt_negative_balance(&mut corrupted4, &fx.user_a);
    match verify_conservation(&before, &corrupted4) {
        Err(InvariantViolation::ImpossibleBalanceDetected { balance, .. }) => {
            assert_eq!(balance, -100, "Caught impossible negative balance");
        }
        other => panic!("Expected ImpossibleBalanceDetected, got {:?}", other),
    }
}
