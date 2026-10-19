#![cfg(test)]

extern crate std;

use super::*;
use shared::Error as SharedError;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Env,
};
use std::collections::BTreeMap;

#[allow(dead_code)]
fn setup_token<'a>(
    env: &'a Env,
    admin: &Address,
) -> (Address, token::Client<'a>, token::StellarAssetClient<'a>) {
    let contract_address = env.register_stellar_asset_contract(admin.clone());
    let client = token::Client::new(env, &contract_address);
    let asset_client = token::StellarAssetClient::new(env, &contract_address);
    (contract_address, client, asset_client)
}

const MINT_AMOUNT: i128 = 1_000_000;

struct Fixture {
    env: Env,
    admin: Address,
    donor: Address,
    recipient: Address,
    token_addr: Address,
    contract_id: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let donor = Address::generate(&env);
    let recipient = Address::generate(&env);

    let token_addr = env.register_stellar_asset_contract(admin.clone());
    let asset_client = token::StellarAssetClient::new(&env, &token_addr);
    asset_client.mint(&donor, &MINT_AMOUNT);

    let contract_id = env.register_contract(None, AidContract);
    let client = AidContractClient::new(&env, &contract_id);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &token_addr, &3600);

    Fixture {
        env,
        admin,
        donor,
        recipient,
        token_addr,
        contract_id,
    }
}

/// Create `count` aids of 100 units each from `donor` to `recipient`,
/// returning the allocated IDs in creation order.
fn create_aids(
    env: &Env,
    client: &AidContractClient,
    donor: &Address,
    recipient: &Address,
    count: u32,
) -> std::vec::Vec<u64> {
    let expiry = env.ledger().sequence() + 10_000;
    let mut ids = std::vec::Vec::with_capacity(count as usize);
    for _ in 0..count {
        ids.push(client.create_aid(donor, recipient, &100, &expiry, &None));
    }
    ids
}

fn advance_ledger(env: &Env, delta: u32) {
    env.ledger().with_mut(|l| {
        l.sequence_number += delta;
    });
}

// ---------------------------------------------------------------------------
// Contract state snapshot helpers
// ---------------------------------------------------------------------------
//
// These helpers capture a deterministic view of the contract's observable
// state (token balances, aid records, search index, pause flag) so that tests
// can assert on *deltas* rather than re-deriving every field by hand.
//
// Usage:
//   let before = ContractSnapshot::capture(&fx);
//   client.claim_aid(&aid_id, &fx.recipient);
//   let after = ContractSnapshot::capture(&fx);
//   before.expect_delta(&after)
//       .balance(&fx.contract_id, -500)
//       .balance(&fx.recipient, 500)
//       .aid_status(aid_id, AidStatus::Settled)
//       .assert();
//
// Any field not explicitly listed in the expectation must be unchanged, and
// any unexpected change fails the test with a readable diff.

/// A point-in-time snapshot of everything a test typically cares about.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ContractSnapshot {
    /// Token balance per address, keyed by the address' string form so the
    /// ordering is deterministic across runs.
    balances: BTreeMap<std::string::String, i128>,
    /// Aid record per id, keyed by id for deterministic ordering.
    aids: BTreeMap<u64, AidRecord>,
    /// Ordered list of ids currently present in the search index.
    search_index: std::vec::Vec<u64>,
    /// Whether the contract is paused.
    paused: bool,
}

impl ContractSnapshot {
    fn capture(fx: &Fixture) -> Self {
        let client = AidContractClient::new(&fx.env, &fx.contract_id);
        let token_client = token::Client::new(&fx.env, &fx.token_addr);

        let mut balances = BTreeMap::new();
        for addr in [&fx.admin, &fx.donor, &fx.recipient, &fx.contract_id] {
            balances.insert(std::format!("{:?}", addr), token_client.balance(addr));
        }

        let mut aids = BTreeMap::new();
        let mut cursor: Option<u64> = None;
        loop {
            let page = client.list_aids_by_donor_cursor(&fx.donor, &cursor, &50);
            for record in page.records.iter() {
                aids.insert(record.id, record.clone());
            }
            if !page.has_more {
                break;
            }
            cursor = page.next_cursor;
        }

        let search_index = fx.env.as_contract(&fx.contract_id, || {
            storage::get_search_index(&fx.env)
                .iter()
                .collect::<std::vec::Vec<u64>>()
        });

        let paused = fx.env.as_contract(&fx.contract_id, || {
            fx.env
                .storage()
                .instance()
                .get::<_, bool>(&Symbol::new(&fx.env, "paused"))
                .unwrap_or(false)
        });

        ContractSnapshot {
            balances,
            aids,
            search_index,
            paused,
        }
    }

    fn expect_delta<'a>(&'a self, after: &'a ContractSnapshot) -> DeltaExpectation<'a> {
        DeltaExpectation {
            before: self,
            after,
            expected_balances: BTreeMap::new(),
            expected_statuses: BTreeMap::new(),
            expected_search_index: None,
            expected_paused: None,
        }
    }
}

/// Fluent builder describing the expected delta between two snapshots.
struct DeltaExpectation<'a> {
    before: &'a ContractSnapshot,
    after: &'a ContractSnapshot,
    expected_balances: BTreeMap<std::string::String, i128>,
    expected_statuses: BTreeMap<u64, Option<AidStatus>>,
    expected_search_index: Option<std::vec::Vec<u64>>,
    expected_paused: Option<bool>,
}

impl<'a> DeltaExpectation<'a> {
    fn balance(mut self, addr: &Address, delta: i128) -> Self {
        self.expected_balances
            .insert(std::format!("{:?}", addr), delta);
        self
    }

    fn aid_status(mut self, aid_id: u64, status: AidStatus) -> Self {
        self.expected_statuses.insert(aid_id, Some(status));
        self
    }

    fn aid_deleted(mut self, aid_id: u64) -> Self {
        self.expected_statuses.insert(aid_id, None);
        self
    }

    fn search_index(mut self, ids: std::vec::Vec<u64>) -> Self {
        self.expected_search_index = Some(ids);
        self
    }

    fn paused(mut self, paused: bool) -> Self {
        self.expected_paused = Some(paused);
        self
    }

    fn assert(self) {
        let mut failures: std::vec::Vec<std::string::String> = std::vec::Vec::new();

        // --- Balances -----------------------------------------------------
        let mut all_addrs: std::collections::BTreeSet<&std::string::String> =
            self.before.balances.keys().collect();
        all_addrs.extend(self.after.balances.keys());
        for addr in all_addrs {
            let before = self.before.balances.get(addr).copied().unwrap_or(0);
            let after = self.after.balances.get(addr).copied().unwrap_or(0);
            let actual_delta = after - before;
            let expected_delta = self.expected_balances.get(addr).copied().unwrap_or(0);
            if actual_delta != expected_delta {
                failures.push(std::format!(
                    "balance delta for {}: expected {}, got {} (before={}, after={})",
                    addr,
                    expected_delta,
                    actual_delta,
                    before,
                    after,
                ));
            }
        }

        // --- Aid statuses -------------------------------------------------
        let mut all_ids: std::collections::BTreeSet<u64> =
            self.before.aids.keys().copied().collect();
        all_ids.extend(self.after.aids.keys().copied());
        for id in all_ids {
            let before_status = self.before.aids.get(&id).map(|r| r.status.clone());
            let after_status = self.after.aids.get(&id).map(|r| r.status.clone());
            let expected = self.expected_statuses.get(&id);
            match expected {
                Some(exp) => {
                    if after_status.as_ref() != exp.as_ref() {
                        failures.push(std::format!(
                            "aid {} status: expected {:?}, got {:?} (before={:?})",
                            id,
                            exp,
                            after_status,
                            before_status,
                        ));
                    }
                }
                None => {
                    if before_status != after_status {
                        failures.push(std::format!(
                            "aid {} status changed unexpectedly: {:?} -> {:?}",
                            id,
                            before_status,
                            after_status,
                        ));
                    }
                }
            }
        }

        // --- Search index -------------------------------------------------
        match self.expected_search_index {
            Some(expected) => {
                if self.after.search_index != expected {
                    failures.push(std::format!(
                        "search index: expected {:?}, got {:?} (before={:?})",
                        expected,
                        self.after.search_index,
                        self.before.search_index,
                    ));
                }
            }
            None => {
                if self.before.search_index != self.after.search_index {
                    failures.push(std::format!(
                        "search index changed unexpectedly: {:?} -> {:?}",
                        self.before.search_index,
                        self.after.search_index,
                    ));
                }
            }
        }

        // --- Paused flag --------------------------------------------------
        match self.expected_paused {
            Some(expected) => {
                if self.after.paused != expected {
                    failures.push(std::format!(
                        "paused flag: expected {}, got {} (before={})",
                        expected,
                        self.after.paused,
                        self.before.paused,
                    ));
                }
            }
            None => {
                if self.before.paused != self.after.paused {
                    failures.push(std::format!(
                        "paused flag changed unexpectedly: {} -> {}",
                        self.before.paused,
                        self.after.paused,
                    ));
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "unexpected contract state delta:\n  - {}",
                failures.join("\n  - ")
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Claim lifecycle
// ---------------------------------------------------------------------------

#[test]
fn claim_transfers_escrow_and_settles() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);
    assert_eq!(token_client.balance(&fx.contract_id), 500);

    let before = ContractSnapshot::capture(&fx);
    client.claim_aid(&aid_id, &fx.recipient);
    let after = ContractSnapshot::capture(&fx);

    assert_eq!(token_client.balance(&fx.recipient), 500);
    assert_eq!(token_client.balance(&fx.contract_id), 0);

    let record = client.get_aid(&aid_id).unwrap();
    assert_eq!(record.status, AidStatus::Settled);

    before
        .expect_delta(&after)
        .balance(&fx.contract_id, -500)
        .balance(&fx.recipient, 500)
        .aid_status(aid_id, AidStatus::Settled)
        .search_index(std::vec::Vec::new())
        .assert();
}

#[test]
fn second_claim_returns_already_claimed() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);
    client.claim_aid(&aid_id, &fx.recipient);

    let result = client.try_claim_aid(&aid_id, &fx.recipient);
    assert_eq!(result, Err(Ok(AidError::AlreadyClaimed)));
}

#[test]
fn claim_after_expiry_is_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);

    advance_ledger(&fx.env, 101);

    let before = ContractSnapshot::capture(&fx);
    let result = client.try_claim_aid(&aid_id, &fx.recipient);
    assert_eq!(result, Err(Ok(AidError::Expired)));
    let after = ContractSnapshot::capture(&fx);
    before.expect_delta(&after).assert();
}

#[test]
fn claim_by_wrong_address_is_unauthorized() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let stranger = Address::generate(&fx.env);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);

    let before = ContractSnapshot::capture(&fx);
    let result = client.try_claim_aid(&aid_id, &stranger);
    assert_eq!(result, Err(Ok(AidError::Unauthorized)));
    let after = ContractSnapshot::capture(&fx);
    before.expect_delta(&after).assert();
}

#[test]
fn claim_while_paused_is_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);
    client.set_paused(&fx.admin, &true);

    let before = ContractSnapshot::capture(&fx);
    let result = client.try_claim_aid(&aid_id, &fx.recipient);
    assert_eq!(result, Err(Ok(AidError::Paused)));
    let after = ContractSnapshot::capture(&fx);
    before.expect_delta(&after).paused(true).assert();
}

#[test]
fn pauser_permission_can_be_granted_and_revoked() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let pauser = Address::generate(&fx.env);

    assert_eq!(
        client.try_set_pauser(&fx.donor, &pauser, &true),
        Err(Ok(SharedError::Unauthorized))
    );
    client.set_pauser(&fx.admin, &pauser, &true);
    client.set_paused(&pauser, &true);
    let paused = fx.env.as_contract(&fx.contract_id, || {
        fx.env
            .storage()
            .instance()
            .get::<_, bool>(&Symbol::new(&fx.env, "paused"))
            .unwrap_or(false)
    });
    assert!(paused);

    client.set_paused(&fx.admin, &false);
    client.set_pauser(&fx.admin, &pauser, &false);
    assert!(client.try_set_paused(&pauser, &true).is_err());
}

#[test]
fn create_aid_rejects_non_positive_amount_and_past_expiry() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    assert_eq!(
        client.try_create_aid(&fx.donor, &fx.recipient, &0, &expiry, &None),
        Err(Ok(soroban_sdk::Error::from_contract_error(
            SharedError::InvalidAmount as u32
        )))
    );

    let past = fx.env.ledger().sequence();
    assert_eq!(
        client.try_create_aid(&fx.donor, &fx.recipient, &100, &past, &None),
        Err(Ok(soroban_sdk::Error::from_contract_error(
            SharedError::InvalidArgument as u32
        )))
    );
}

// ---------------------------------------------------------------------------
// Refunds
// ---------------------------------------------------------------------------

#[test]
fn refund_aid_after_expiry_returns_funds_to_donor() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);
    advance_ledger(&fx.env, 101);

    let before = ContractSnapshot::capture(&fx);
    client.refund_aid(&aid_id, &fx.donor);
    let after = ContractSnapshot::capture(&fx);

    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT);
    assert_eq!(token_client.balance(&fx.contract_id), 0);
    let record = client.get_aid(&aid_id).unwrap();
    assert_eq!(record.status, AidStatus::Refunded);

    before
        .expect_delta(&after)
        .balance(&fx.contract_id, -500)
        .balance(&fx.donor, 500)
        .aid_status(aid_id, AidStatus::Refunded)
        .search_index(std::vec::Vec::new())
        .assert();
}

#[test]
fn refund_aid_before_expiry_is_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);

    let before = ContractSnapshot::capture(&fx);
    let result = client.try_refund_aid(&aid_id, &fx.donor);
    assert_eq!(result, Err(Ok(AidError::NotExpiredYet)));
    let after = ContractSnapshot::capture(&fx);
    before.expect_delta(&after).assert();
}

#[test]
fn refund_claimed_aid_is_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);
    client.claim_aid(&aid_id, &fx.recipient);
    advance_ledger(&fx.env, 101);

    let before = ContractSnapshot::capture(&fx);
    let result = client.try_refund_aid(&aid_id, &fx.donor);
    assert_eq!(result, Err(Ok(AidError::AlreadyClaimed)));
    let after = ContractSnapshot::capture(&fx);
    before.expect_delta(&after).assert();
}

#[test]
fn refund_refunded_aid_is_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);
    advance_ledger(&fx.env, 101);
    client.refund_aid(&aid_id, &fx.donor);

    let before = ContractSnapshot::capture(&fx);
    let result = client.try_refund_aid(&aid_id, &fx.donor);
    assert_eq!(result, Err(Ok(AidError::AlreadyRefunded)));
    let after = ContractSnapshot::capture(&fx);
    before.expect_delta(&after).assert();
}

#[test]
fn refund_by_admin_is_successful() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &500, &expiry, &None);
    advance_ledger(&fx.env, 101);

    let before = ContractSnapshot::capture(&fx);
    client.refund_aid(&aid_id, &fx.admin);
    let after = ContractSnapshot::capture(&fx);

    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT);
    let record = client.get_aid(&aid_id).unwrap();
    assert_eq!(record.status, AidStatus::Refunded);

    before
        .expect_delta(&after)
        .balance(&fx.contract_id, -500)
        .balance(&fx.donor, 500)
        .aid_status(aid_id, AidStatus::Refunded)
        .search_index(std::vec::Vec::new())
        .assert();
}

// ---------------------------------------------------------------------------
// Single-record queries
// ---------------------------------------------------------------------------

#[test]
fn get_aid_unknown_id_returns_none() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    assert_eq!(client.get_aid(&9_999), None);
}

#[test]
fn get_aid_returns_full_record() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let expiry = fx.env.ledger().sequence() + 100;
    let aid_id = client.create_aid(&fx.donor, &fx.recipient, &250, &expiry, &None);

    let record = client.get_aid(&aid_id).expect("record should exist");
    assert_eq!(record.id, aid_id);
    assert_eq!(record.donor, fx.donor);
    assert_eq!(record.recipient, fx.recipient);
    assert_eq!(record.token, fx.token_addr);
    assert_eq!(record.amount, 250);
    assert_eq!(record.expiry_ledger, expiry);
    assert_eq!(record.status, AidStatus::Pending);
}

#[test]
fn aid_ids_are_unique_and_monotonic() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let ids = create_aids(&fx.env, &client, &fx.donor, &fx.recipient, 5);
    let sorted = ids.clone();
    assert_eq!(ids, sorted);
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(*id, i as u64);
    }
}

// ---------------------------------------------------------------------------
// Permission-aware discovery index
// ---------------------------------------------------------------------------

#[test]
fn search_filters_restricted_records_and_honors_permission_revocation() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let outsider = Address::generate(&fx.env);
    let delegate = Address::generate(&fx.env);
    let aid_id = client.create_aid(
        &fx.donor,
        &fx.recipient,
        &100,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );

    assert_eq!(client.search_aids(&outsider, &0, &10).records.len(), 0);
    assert_eq!(client.search_aids(&fx.recipient, &0, &10).records.len(), 1);

    client.grant_search_access(&fx.donor, &aid_id, &delegate);
    assert_eq!(client.search_aids(&delegate, &0, &10).records.len(), 1);
    client.revoke_search_access(&fx.donor, &aid_id, &delegate);
    assert_eq!(client.search_aids(&delegate, &0, &10).records.len(), 0);
}

#[test]
fn visibility_change_and_deletion_remove_discovery_entries() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let aid_id = client.create_aid(
        &fx.donor,
        &fx.recipient,
        &100,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );

    let before = ContractSnapshot::capture(&fx);
    client.set_aid_search_visibility(&fx.admin, &aid_id, &false);
    assert_eq!(client.search_aids(&fx.donor, &0, &10).records.len(), 0);
    let after = ContractSnapshot::capture(&fx);
    before
        .expect_delta(&after)
        .search_index(std::vec::Vec::new())
        .assert();

    let before = ContractSnapshot::capture(&fx);
    client.set_aid_search_visibility(&fx.admin, &aid_id, &true);
    assert_eq!(client.search_aids(&fx.donor, &0, &10).records.len(), 1);
    let after = ContractSnapshot::capture(&fx);
    before
        .expect_delta(&after)
        .search_index(std::vec![aid_id])
        .assert();

    let before = ContractSnapshot::capture(&fx);
    client.claim_aid(&aid_id, &fx.recipient);
    assert_eq!(client.search_aids(&fx.donor, &0, &10).records.len(), 0);
    let after = ContractSnapshot::capture(&fx);
    before
        .expect_delta(&after)
        .balance(&fx.contract_id, -100)
        .balance(&fx.recipient, 100)
        .aid_status(aid_id, AidStatus::Settled)
        .search_index(std::vec::Vec::new())
        .assert();

    let before = ContractSnapshot::capture(&fx);
    client.delete_aid(&fx.admin, &aid_id);
    assert_eq!(client.get_aid(&aid_id), None);
    let after = ContractSnapshot::capture(&fx);
    before
        .expect_delta(&after)
        .aid_deleted(aid_id)
        .search_index(std::vec::Vec::new())
        .assert();
}

#[test]
fn repair_search_index_restores_missing_entries_and_removes_stale_ones() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let aid_id = client.create_aid(
        &fx.donor,
        &fx.recipient,
        &100,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );

    // Simulate a partial indexer write and a dangling entry from evicted data.
    fx.env.as_contract(&fx.contract_id, || {
        let mut corrupt = Vec::new(&fx.env);
        corrupt.push_back(99_999);
        storage::set_search_index(&fx.env, &corrupt);
    });

    let report = client.repair_search_index(&fx.admin);
    assert_eq!(report.indexed, 1);
    assert_eq!(report.added, 1);
    assert_eq!(report.removed, 1);
    let results = client.search_aids(&fx.donor, &0, &10);
    assert_eq!(results.records.len(), 1);
    assert_eq!(results.records.get(0).unwrap().id, aid_id);
}

#[test]
fn test_stable_cursor_pagination_on_aid_contract() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    // Create 5 aids
    let mut ids = std::vec::Vec::new();
    let expiry = fx.env.ledger().sequence() + 1000;
    for _ in 0..5 {
        ids.push(client.create_aid(&fx.donor, &fx.recipient, &100, &expiry, &None));
    }

    // Page 1: limit 2
    let page1 = client.list_aids_by_donor_cursor(&fx.donor, &None, &2);
    assert_eq!(page1.records.len(), 2);
    assert_eq!(page1.records.get(0).unwrap().id, ids[0]);
    assert_eq!(page1.records.get(1).unwrap().id, ids[1]);
    assert_eq!(page1.next_cursor, Some(ids[1]));
    assert!(page1.has_more);

    // Page 2: limit 2, start_after_id = ids[1]
    let page2 = client.list_aids_by_donor_cursor(&fx.donor, &page1.next_cursor, &2);
    assert_eq!(page2.records.len(), 2);
    assert_eq!(page2.records.get(0).unwrap().id, ids[2]);
    assert_eq!(page2.records.get(1).unwrap().id, ids[3]);
    assert_eq!(page2.next_cursor, Some(ids[3]));
    assert!(page2.has_more);

    // Page 3: limit 2, start_after_id = ids[3]
    let page3 = client.list_aids_by_donor_cursor(&fx.donor, &page2.next_cursor, &2);
    assert_eq!(page3.records.len(), 1);
    assert_eq!(page3.records.get(0).unwrap().id, ids[4]);
    assert_eq!(page3.next_cursor, None);
    assert!(!page3.has_more);

    // Test search_aids_cursor
    let search_page = client.search_aids_cursor(&fx.donor, &None, &3);
    assert_eq!(search_page.records.len(), 3);
    assert_eq!(search_page.next_cursor, Some(ids[2]));
}

#[test]
fn test_cursor_pagination_under_inserts_deletes_and_permission_changes() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let delegate = Address::generate(&fx.env);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut ids = std::vec::Vec::new();
    for _ in 0..6 {
        let id = client.create_aid(&fx.donor, &fx.recipient, &100, &expiry, &None);
        client.grant_search_access(&fx.donor, &id, &delegate);
        ids.push(id);
    }

    // Page 1: delegate reads 2 items -> ids[0], ids[1]
    let page1 = client.search_aids_cursor(&delegate, &None, &2);
    assert_eq!(page1.records.len(), 2);
    assert_eq!(page1.records.get(0).unwrap().id, ids[0]);
    assert_eq!(page1.records.get(1).unwrap().id, ids[1]);
    assert_eq!(page1.next_cursor, Some(ids[1]));
    assert!(page1.has_more);

    // Dynamic mutations and permission changes before page 2:
    // 1. Claim/settle ids[2] (removes from active pending search)
    client.claim_aid(&ids[2], &fx.recipient);

    // 2. Hide ids[3] by admin
    client.set_aid_search_visibility(&fx.admin, &ids[3], &false);

    // 3. Revoke delegate's discovery permission on ids[4]
    client.revoke_search_access(&fx.donor, &ids[4], &delegate);

    // 4. Concurrently insert 2 new aids (ids[6], ids[7]) and grant search access
    let id6 = client.create_aid(&fx.donor, &fx.recipient, &100, &expiry, &None);
    client.grant_search_access(&fx.donor, &id6, &delegate);
    let id7 = client.create_aid(&fx.donor, &fx.recipient, &100, &expiry, &None);
    client.grant_search_access(&fx.donor, &id7, &delegate);

    // Page 2: with cursor ids[1], limit 2
    // Must skip ids[2] (claimed), ids[3] (hidden), ids[4] (permission revoked)
    // and return ids[5] and id6
    let page2 = client.search_aids_cursor(&delegate, &page1.next_cursor, &2);
    assert_eq!(page2.records.len(), 2);
    assert_eq!(page2.records.get(0).unwrap().id, ids[5]);
    assert_eq!(page2.records.get(1).unwrap().id, id6);
    assert_eq!(page2.next_cursor, Some(id6));
    assert!(page2.has_more);

    // Page 3: with cursor id6, limit 2
    // Must return id7 and terminate
    let page3 = client.search_aids_cursor(&delegate, &page2.next_cursor, &2);
    assert_eq!(page3.records.len(), 1);
    assert_eq!(page3.records.get(0).unwrap().id, id7);
    assert_eq!(page3.next_cursor, None);
    assert!(!page3.has_more);
}

#[test]
fn test_aid_contract_import_dry_run_and_commit() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let ext_1 = Bytes::from_slice(&fx.env, b"AID-EXT-01");
    let ext_2 = Bytes::from_slice(&fx.env, b"AID-EXT-02");

    let mut items = Vec::new(&fx.env);
    items.push_back(shared::import::ImportItem {
        row_id: 0,
        external_id: Some(ext_1.clone()),
        recipient: fx.recipient.clone(),
        amount: 500,
        expiry: fx.env.ledger().timestamp() + 3600,
        metadata_hash: None,
    });
    items.push_back(shared::import::ImportItem {
        row_id: 1,
        external_id: Some(ext_2.clone()),
        recipient: fx.recipient.clone(),
        amount: 800,
        expiry: fx.env.ledger().timestamp() + 7200,
        metadata_hash: None,
    });

    let config = shared::import::ImportConfig {
        dry_run: false,
        mode: shared::import::ImportMode::AllOrNothing,
        duplicate_policy: shared::import::DuplicatePolicy::SkipExisting,
        max_rows: 50,
    };

    // 1. Dry run via contract entrypoint
    let dry_report = client.import_aids_dry_run(&items, &config);
    assert!(dry_report.is_dry_run);
    assert_eq!(dry_report.total_rows, 2);
    assert_eq!(dry_report.create_count, 2);
    assert_eq!(dry_report.error_count, 0);

    // Acceptance criteria check: Dry run performs no persistent writes
    assert_eq!(client.get_imported_aid(&ext_1), None);
    assert_eq!(client.get_imported_aid(&ext_2), None);

    // 2. Commit execution via contract entrypoint
    let commit_report = client.import_aids(&fx.donor, &items, &config);
    assert!(!commit_report.is_dry_run);
    assert_eq!(commit_report.create_count, 2);
    assert_eq!(commit_report.error_count, 0);

    // Verify stored records exist
    let rec1 = client
        .get_imported_aid(&ext_1)
        .expect("rec1 should be persisted");
    assert_eq!(rec1.amount, 500);
    assert_eq!(rec1.recipient, fx.recipient);

    let rec2 = client
        .get_imported_aid(&ext_2)
        .expect("rec2 should be persisted");
    assert_eq!(rec2.amount, 800);

    // 3. Acceptance criteria check: Repeated imports are idempotent where external IDs are present
    let rerun_report = client.import_aids(&fx.donor, &items, &config);
    assert_eq!(rerun_report.create_count, 0);
    assert_eq!(rerun_report.skip_count, 2);
    assert_eq!(rerun_report.update_count, 0);
    assert_eq!(rerun_report.error_count, 0);
}

#[test]
fn test_aid_contract_import_partial_failure_handling() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let ext_good = Bytes::from_slice(&fx.env, b"AID-GOOD");
    let ext_bad = Bytes::from_slice(&fx.env, b"AID-BAD");

    let mut items = Vec::new(&fx.env);
    items.push_back(shared::import::ImportItem {
        row_id: 0,
        external_id: Some(ext_good.clone()),
        recipient: fx.recipient.clone(),
        amount: 300,
        expiry: fx.env.ledger().timestamp() + 5000,
        metadata_hash: None,
    });
    items.push_back(shared::import::ImportItem {
        row_id: 1,
        external_id: Some(ext_bad.clone()),
        recipient: fx.recipient.clone(),
        amount: 0, // Invalid amount: triggers row failure
        expiry: fx.env.ledger().timestamp() + 5000,
        metadata_hash: None,
    });

    let config = shared::import::ImportConfig {
        dry_run: false,
        mode: shared::import::ImportMode::BestEffort,
        duplicate_policy: shared::import::DuplicatePolicy::SkipExisting,
        max_rows: 50,
    };

    let report = client.import_aids(&fx.donor, &items, &config);
    assert_eq!(report.total_rows, 2);
    assert_eq!(report.create_count, 1);
    assert_eq!(report.error_count, 1);
    assert_eq!(report.errors.len(), 1);

    // Check error details and rollback guidance
    let err = report.errors.get(0).unwrap();
    assert_eq!(err.row_id, 1);
    assert_eq!(err.reason, symbol_short!("zero_amt"));
    assert_eq!(
        report.rollback_guidance.strategy,
        shared::import::RollbackStrategy::ForwardFix
    );
    assert_eq!(report.rollback_guidance.action, symbol_short!("part_fix"));

    // Valid row is committed, invalid row is not
    assert!(client.get_imported_aid(&ext_good).is_some());
    assert!(client.get_imported_aid(&ext_bad).is_none());
}

// ---------------------------------------------------------------------------
// Issue #140: Deterministic Storage Schema Version Guards Acceptance Tests
// ---------------------------------------------------------------------------

#[test]
fn test_storage_schema_guard_current_succeeds() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    // Initialized contract stamps CURRENT_STORAGE_SCHEMA_VERSION (2)
    assert_eq!(
        client.get_storage_schema_version(),
        shared::storage_version::CURRENT_STORAGE_SCHEMA_VERSION
    );

    // Schema validation succeeds on current version
    assert_eq!(
        client.validate_storage_schema(),
        shared::storage_version::CURRENT_STORAGE_SCHEMA_VERSION
    );

    // Create an aid record
    let aid_id = client.create_aid(
        &fx.donor,
        &fx.recipient,
        &500,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );

    // Guarded read succeeds and returns latest record
    let guarded = client.get_aid_guarded(&aid_id);
    assert!(guarded.is_some());
    let rec = guarded.unwrap();
    assert_eq!(rec.id, aid_id);
    assert_eq!(rec.amount, 500);
    assert_eq!(
        rec.schema_version,
        shared::compat::CURRENT_RECORD_SCHEMA_VERSION
    );
}

#[test]
fn test_storage_schema_guard_missing_version_fails_safely() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let aid_id = client.create_aid(
        &fx.donor,
        &fx.recipient,
        &500,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );

    // Simulate missing schema version (e.g. unmigrated legacy contract)
    fx.env.as_contract(&fx.contract_id, || {
        shared::storage_version::remove_storage_schema_version(&fx.env);
    });

    // Guarded read fails safely with UnsupportedSchemaVersion
    let read_res = client.try_get_aid_guarded(&aid_id);
    assert_eq!(read_res, Err(Ok(AidError::UnsupportedSchemaVersion)));

    // Validation fails safely with UnsupportedSchemaVersion
    let val_res = client.try_validate_storage_schema();
    assert_eq!(val_res, Err(Ok(AidError::UnsupportedSchemaVersion)));
}

#[test]
fn test_storage_schema_guard_old_compatible_version_succeeds_and_upgrades() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let aid_id = client.create_aid(
        &fx.donor,
        &fx.recipient,
        &500,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );

    // Set schema version to V1 (old-compatible)
    fx.env.as_contract(&fx.contract_id, || {
        shared::storage_version::set_storage_schema_version(
            &fx.env,
            shared::storage_version::STORAGE_SCHEMA_V1,
        );
    });

    assert_eq!(
        client.get_storage_schema_version(),
        shared::storage_version::STORAGE_SCHEMA_V1
    );

    // Old compatible schema read succeeds
    assert_eq!(
        client.validate_storage_schema(),
        shared::storage_version::STORAGE_SCHEMA_V1
    );
    let rec = client.get_aid_guarded(&aid_id).unwrap();
    assert_eq!(rec.id, aid_id);

    // Upgrade schema from V1 to V2 succeeds
    let upg = client.upgrade_storage_schema(
        &fx.admin,
        &shared::storage_version::CURRENT_STORAGE_SCHEMA_VERSION,
    );
    assert_eq!(upg, shared::storage_version::CURRENT_STORAGE_SCHEMA_VERSION);
    assert_eq!(
        client.get_storage_schema_version(),
        shared::storage_version::CURRENT_STORAGE_SCHEMA_VERSION
    );

    // Non-admin upgrade rejected
    let unauth_res = client.try_upgrade_storage_schema(
        &fx.recipient,
        &shared::storage_version::CURRENT_STORAGE_SCHEMA_VERSION,
    );
    assert_eq!(unauth_res, Err(Ok(AidError::Unauthorized)));
}

#[test]
fn test_storage_schema_guard_incompatible_versions_fail_safely() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    let aid_id = client.create_aid(
        &fx.donor,
        &fx.recipient,
        &500,
        &(fx.env.ledger().sequence() + 100),
        &None,
    );

    // Case 1: Version 0 (invalid/unsupported)
    fx.env.as_contract(&fx.contract_id, || {
        shared::storage_version::set_storage_schema_version(&fx.env, 0);
    });
    assert_eq!(
        client.try_validate_storage_schema(),
        Err(Ok(AidError::UnsupportedSchemaVersion))
    );
    assert_eq!(
        client.try_get_aid_guarded(&aid_id),
        Err(Ok(AidError::UnsupportedSchemaVersion))
    );

    // Case 2: Future version 99 (incompatible)
    fx.env.as_contract(&fx.contract_id, || {
        shared::storage_version::set_storage_schema_version(&fx.env, 99);
    });
    assert_eq!(
        client.try_validate_storage_schema(),
        Err(Ok(AidError::UnsupportedSchemaVersion))
    );
    assert_eq!(
        client.try_get_aid_guarded(&aid_id),
        Err(Ok(AidError::UnsupportedSchemaVersion))
    );

    // Upgrading to unsupported version is rejected
    assert_eq!(
        client.try_upgrade_storage_schema(&fx.admin, &100),
        Err(Ok(AidError::UnsupportedSchemaVersion))
    );
}

// ---------------------------------------------------------------------------
// Batch Aid Creation Tests (Issue #178)
// ---------------------------------------------------------------------------

#[test]
fn test_batch_create_aid_success() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let initial_donor_balance = token_client.balance(&fx.donor);
    assert_eq!(initial_donor_balance, MINT_AMOUNT);
    assert_eq!(token_client.balance(&fx.contract_id), 0);

    let r1 = Address::generate(&fx.env);
    let r2 = Address::generate(&fx.env);
    let r3 = Address::generate(&fx.env);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut allocations = Vec::new(&fx.env);
    allocations.push_back(BatchAidAllocation {
        recipient: r1.clone(),
        amount: 150,
        purpose: symbol_short!("food"),
        expiry_ledger: expiry,
    });
    allocations.push_back(BatchAidAllocation {
        recipient: r2.clone(),
        amount: 250,
        purpose: symbol_short!("medical"),
        expiry_ledger: expiry,
    });
    allocations.push_back(BatchAidAllocation {
        recipient: r3.clone(),
        amount: 350,
        purpose: symbol_short!("shelter"),
        expiry_ledger: expiry + 50,
    });

    let total_amount: i128 = 150 + 250 + 350;

    let ids = client.batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(ids.len(), 3);
    assert_eq!(ids.get(0).unwrap(), 0);
    assert_eq!(ids.get(1).unwrap(), 1);
    assert_eq!(ids.get(2).unwrap(), 2);

    // Single consolidated token transfer verified
    assert_eq!(
        token_client.balance(&fx.donor),
        initial_donor_balance - total_amount
    );
    assert_eq!(token_client.balance(&fx.contract_id), total_amount);

    // Sequential records populated correctly
    let rec0 = client.get_aid(&0).expect("record 0 must exist");
    assert_eq!(rec0.id, 0);
    assert_eq!(rec0.donor, fx.donor);
    assert_eq!(rec0.recipient, r1);
    assert_eq!(rec0.token, fx.token_addr);
    assert_eq!(rec0.amount, 150);
    assert_eq!(rec0.expiry_ledger, expiry);
    assert_eq!(rec0.status, AidStatus::Pending);

    let rec1 = client.get_aid(&1).expect("record 1 must exist");
    assert_eq!(rec1.id, 1);
    assert_eq!(rec1.donor, fx.donor);
    assert_eq!(rec1.recipient, r2);
    assert_eq!(rec1.token, fx.token_addr);
    assert_eq!(rec1.amount, 250);
    assert_eq!(rec1.expiry_ledger, expiry);
    assert_eq!(rec1.status, AidStatus::Pending);

    let rec2 = client.get_aid(&2).expect("record 2 must exist");
    assert_eq!(rec2.id, 2);
    assert_eq!(rec2.donor, fx.donor);
    assert_eq!(rec2.recipient, r3);
    assert_eq!(rec2.token, fx.token_addr);
    assert_eq!(rec2.amount, 350);
    assert_eq!(rec2.expiry_ledger, expiry + 50);
    assert_eq!(rec2.status, AidStatus::Pending);

    // Donor and recipient indexes populated
    fx.env.as_contract(&fx.contract_id, || {
        let donor_aids = storage::get_donor_aids(&fx.env, &fx.donor);
        assert_eq!(donor_aids.len(), 3);
        assert_eq!(donor_aids.get(0).unwrap(), 0);
        assert_eq!(donor_aids.get(1).unwrap(), 1);
        assert_eq!(donor_aids.get(2).unwrap(), 2);

        let r1_aids = storage::get_recipient_aids(&fx.env, &r1);
        assert_eq!(r1_aids.len(), 1);
        assert_eq!(r1_aids.get(0).unwrap(), 0);

        let r2_aids = storage::get_recipient_aids(&fx.env, &r2);
        assert_eq!(r2_aids.len(), 1);
        assert_eq!(r2_aids.get(0).unwrap(), 1);

        let r3_aids = storage::get_recipient_aids(&fx.env, &r3);
        assert_eq!(r3_aids.len(), 1);
        assert_eq!(r3_aids.get(0).unwrap(), 2);
    });

    // Recipients can claim their aids
    client.claim_aid(&0, &r1);
    assert_eq!(token_client.balance(&r1), 150);
    assert_eq!(token_client.balance(&fx.contract_id), total_amount - 150);
    assert_eq!(client.get_aid(&0).unwrap().status, AidStatus::Settled);
}

#[test]
fn test_batch_create_aid_max_batch_size_50() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut allocations = Vec::new(&fx.env);

    for _ in 0..MAX_BATCH_SIZE {
        let recipient = Address::generate(&fx.env);
        allocations.push_back(BatchAidAllocation {
            recipient,
            amount: 10,
            purpose: symbol_short!("grant"),
            expiry_ledger: expiry,
        });
    }

    assert_eq!(allocations.len(), 50);
    let ids = client.batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(ids.len(), 50);
    for i in 0..50 {
        assert_eq!(ids.get(i).unwrap(), i as u64);
    }

    assert_eq!(token_client.balance(&fx.contract_id), 500);
    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT - 500);
}

#[test]
fn test_batch_create_aid_exceeds_max_batch_size_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut allocations = Vec::new(&fx.env);

    for _ in 0..=MAX_BATCH_SIZE {
        let recipient = Address::generate(&fx.env);
        allocations.push_back(BatchAidAllocation {
            recipient,
            amount: 10,
            purpose: symbol_short!("grant"),
            expiry_ledger: expiry,
        });
    }

    assert_eq!(allocations.len(), 51);
    let res = client.try_batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(res, Err(Ok(AidError::BatchTooLarge)));

    // State is pristine
    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT);
    assert_eq!(token_client.balance(&fx.contract_id), 0);
    assert_eq!(client.get_aid(&0), None);
    fx.env.as_contract(&fx.contract_id, || {
        assert_eq!(storage::get_aid_counter(&fx.env), 0);
        assert_eq!(storage::get_donor_aids(&fx.env, &fx.donor).len(), 0);
    });
}

#[test]
fn test_batch_create_aid_empty_batch_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let allocations = Vec::new(&fx.env);
    let res = client.try_batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(res, Err(Ok(AidError::EmptyBatch)));

    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT);
    assert_eq!(token_client.balance(&fx.contract_id), 0);
    fx.env.as_contract(&fx.contract_id, || {
        assert_eq!(storage::get_aid_counter(&fx.env), 0);
    });
}

#[test]
fn test_batch_create_aid_invalid_token_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let wrong_token = Address::generate(&fx.env);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut allocations = Vec::new(&fx.env);
    allocations.push_back(BatchAidAllocation {
        recipient: Address::generate(&fx.env),
        amount: 100,
        purpose: symbol_short!("food"),
        expiry_ledger: expiry,
    });

    let res = client.try_batch_create_aid(&fx.donor, &wrong_token, &allocations);
    assert_eq!(res, Err(Ok(AidError::InvalidArgument)));
}

#[test]
fn test_batch_create_aid_atomicity_and_rollback_on_invalid_amount() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut allocations = Vec::new(&fx.env);

    // 3 valid entries, 1 invalid (0 amount), 1 valid
    for i in 0..5 {
        let recipient = Address::generate(&fx.env);
        let amount = if i == 3 { 0 } else { 100 };
        allocations.push_back(BatchAidAllocation {
            recipient,
            amount,
            purpose: symbol_short!("relief"),
            expiry_ledger: expiry,
        });
    }

    let res = client.try_batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(res, Err(Ok(AidError::InvalidArgument)));

    // Full atomic rollback verification:
    // No funds moved, no counter incremented, no records created
    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT);
    assert_eq!(token_client.balance(&fx.contract_id), 0);
    for i in 0..5 {
        assert_eq!(client.get_aid(&i), None);
    }
    fx.env.as_contract(&fx.contract_id, || {
        assert_eq!(storage::get_aid_counter(&fx.env), 0);
        assert_eq!(storage::get_donor_aids(&fx.env, &fx.donor).len(), 0);
        assert_eq!(storage::get_search_index(&fx.env).len(), 0);
    });
}

#[test]
fn test_batch_create_aid_atomicity_and_rollback_on_negative_amount() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut allocations = Vec::new(&fx.env);
    allocations.push_back(BatchAidAllocation {
        recipient: Address::generate(&fx.env),
        amount: 100,
        purpose: symbol_short!("relief"),
        expiry_ledger: expiry,
    });
    allocations.push_back(BatchAidAllocation {
        recipient: Address::generate(&fx.env),
        amount: -50,
        purpose: symbol_short!("relief"),
        expiry_ledger: expiry,
    });

    let res = client.try_batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(res, Err(Ok(AidError::InvalidArgument)));

    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT);
    assert_eq!(token_client.balance(&fx.contract_id), 0);
    assert_eq!(client.get_aid(&0), None);
}

#[test]
fn test_batch_create_aid_atomicity_and_rollback_on_expired_ledger() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    let current = fx.env.ledger().sequence();
    let mut allocations = Vec::new(&fx.env);

    // First allocation valid
    allocations.push_back(BatchAidAllocation {
        recipient: Address::generate(&fx.env),
        amount: 100,
        purpose: symbol_short!("aid"),
        expiry_ledger: current + 100,
    });
    // Second allocation has expired ledger
    allocations.push_back(BatchAidAllocation {
        recipient: Address::generate(&fx.env),
        amount: 200,
        purpose: symbol_short!("aid"),
        expiry_ledger: current, // <= current_ledger is invalid
    });

    let res = client.try_batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(res, Err(Ok(AidError::InvalidArgument)));

    // Revert verified
    assert_eq!(token_client.balance(&fx.donor), MINT_AMOUNT);
    assert_eq!(token_client.balance(&fx.contract_id), 0);
    assert_eq!(client.get_aid(&0), None);
    fx.env.as_contract(&fx.contract_id, || {
        assert_eq!(storage::get_aid_counter(&fx.env), 0);
    });
}

#[test]
fn test_batch_create_aid_paused_contract_rejected() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    client.set_paused(&fx.admin, &true);

    let expiry = fx.env.ledger().sequence() + 1000;
    let mut allocations = Vec::new(&fx.env);
    allocations.push_back(BatchAidAllocation {
        recipient: Address::generate(&fx.env),
        amount: 100,
        purpose: symbol_short!("food"),
        expiry_ledger: expiry,
    });

    let res = client.try_batch_create_aid(&fx.donor, &fx.token_addr, &allocations);
    assert_eq!(res, Err(Ok(AidError::Paused)));
}

#[test]
fn test_batch_create_aid_fuzz_validation_and_atomicity() {
    // Deterministic pseudo-random fuzz runner testing varying batch configurations
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);
    let token_client = token::Client::new(&fx.env, &fx.token_addr);

    // Simple pseudo-random LCG for deterministic test generation
    let mut seed: u64 = 0xdeadbeef_cafebabe;
    let mut next_rand = |max: u32| -> u32 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 32) as u32) % max
    };

    let mut expected_created_total: u64 = 0;
    let mut expected_contract_balance: i128 = 0;

    for iteration in 0..25 {
        let size = next_rand(60); // 0..59
        let inject_bad_amount = (iteration % 3) == 0;
        let inject_bad_expiry = (iteration % 5) == 0;
        let bad_index = if size > 0 { next_rand(size) } else { 0 };

        let current_ledger = fx.env.ledger().sequence();
        let mut allocations = Vec::new(&fx.env);
        let mut batch_sum: i128 = 0;

        for i in 0..size {
            let amount: i128 = if inject_bad_amount && i == bad_index {
                if (iteration % 2) == 0 { 0 } else { -10 }
            } else {
                ((next_rand(100) + 1) * 10) as i128
            };

            let expiry_ledger: u32 = if inject_bad_expiry && i == bad_index {
                current_ledger // expired
            } else {
                current_ledger + 100 + next_rand(1000)
            };

            allocations.push_back(BatchAidAllocation {
                recipient: Address::generate(&fx.env),
                amount,
                purpose: symbol_short!("test"),
                expiry_ledger,
            });

            batch_sum += amount;
        }

        let is_valid = size > 0 && size <= MAX_BATCH_SIZE && !inject_bad_amount && !inject_bad_expiry;

        let donor_balance_before = token_client.balance(&fx.donor);
        let contract_balance_before = token_client.balance(&fx.contract_id);
        let counter_before = fx.env.as_contract(&fx.contract_id, || {
            storage::get_aid_counter(&fx.env)
        });

        let res = client.try_batch_create_aid(&fx.donor, &fx.token_addr, &allocations);

        if is_valid {
            assert!(res.is_ok(), "Valid batch must succeed at iteration {}", iteration);
            let ids = res.unwrap().unwrap();
            assert_eq!(ids.len(), size);

            expected_contract_balance += batch_sum;
            assert_eq!(token_client.balance(&fx.contract_id), expected_contract_balance);
            assert_eq!(token_client.balance(&fx.donor), donor_balance_before - batch_sum);

            for i in 0..size {
                let id = ids.get(i).unwrap();
                assert_eq!(id, expected_created_total + i as u64);
                let rec = client.get_aid(&id).expect("record must exist");
                assert_eq!(rec.amount, allocations.get(i).unwrap().amount);
                assert_eq!(rec.status, AidStatus::Pending);
            }
            expected_created_total += size as u64;
        } else {
            // Must have failed and rolled back completely
            assert!(res.is_err(), "Invalid batch must fail at iteration {}", iteration);
            assert_eq!(
                token_client.balance(&fx.donor),
                donor_balance_before,
                "Donor balance must be unchanged on failure at iteration {}",
                iteration
            );
            assert_eq!(
                token_client.balance(&fx.contract_id),
                contract_balance_before,
                "Contract balance must be unchanged on failure at iteration {}",
                iteration
            );
            let counter_after = fx.env.as_contract(&fx.contract_id, || {
                storage::get_aid_counter(&fx.env)
            });
            assert_eq!(
                counter_after,
                counter_before,
                "Counter must not advance on failure at iteration {}",
                iteration
            );
        }
    }
}

#[test]
fn test_stress_bucketed_pagination_no_unbounded_vectors() {
    let fx = setup();
    let client = AidContractClient::new(&fx.env, &fx.contract_id);

    // Create 125 aids (50 in page 0, 50 in page 1, 25 in page 2)
    let ids = create_aids(&fx.env, &client, &fx.donor, &fx.recipient, 125);
    assert_eq!(ids.len(), 125);

    fx.env.as_contract(&fx.contract_id, || {
        // Donor aids pagination checks
        let donor_count = storage::get_donor_aid_count(&fx.env, &fx.donor);
        assert_eq!(donor_count, 125);

        let page0 = storage::get_donor_aids_page(&fx.env, &fx.donor, 0);
        let page1 = storage::get_donor_aids_page(&fx.env, &fx.donor, 1);
        let page2 = storage::get_donor_aids_page(&fx.env, &fx.donor, 2);
        let page3 = storage::get_donor_aids_page(&fx.env, &fx.donor, 3);

        assert_eq!(page0.len(), 50);
        assert_eq!(page1.len(), 50);
        assert_eq!(page2.len(), 25);
        assert_eq!(page3.len(), 0);

        let all_donor_aids = storage::get_donor_aids(&fx.env, &fx.donor);
        assert_eq!(all_donor_aids.len(), 125);

        // Recipient aids pagination checks
        let recipient_count = storage::get_recipient_aid_count(&fx.env, &fx.recipient);
        assert_eq!(recipient_count, 125);

        let r_page0 = storage::get_recipient_aids_page(&fx.env, &fx.recipient, 0);
        let r_page1 = storage::get_recipient_aids_page(&fx.env, &fx.recipient, 1);
        let r_page2 = storage::get_recipient_aids_page(&fx.env, &fx.recipient, 2);
        let r_page3 = storage::get_recipient_aids_page(&fx.env, &fx.recipient, 3);

        assert_eq!(r_page0.len(), 50);
        assert_eq!(r_page1.len(), 50);
        assert_eq!(r_page2.len(), 25);
        assert_eq!(r_page3.len(), 0);

        let all_recipient_aids = storage::get_recipient_aids(&fx.env, &fx.recipient);
        assert_eq!(all_recipient_aids.len(), 125);

        // Search index bucketing checks
        let mut index_vec = soroban_sdk::Vec::new(&fx.env);
        for id in 0..125u64 {
            index_vec.push_back(id);
        }
        storage::set_search_index(&fx.env, &index_vec);

        let search_count = storage::get_search_index_count(&fx.env);
        assert_eq!(search_count, 125);

        let s_page0 = storage::get_search_index_page(&fx.env, 0);
        let s_page1 = storage::get_search_index_page(&fx.env, 1);
        let s_page2 = storage::get_search_index_page(&fx.env, 2);

        assert_eq!(s_page0.len(), 50);
        assert_eq!(s_page1.len(), 50);
        assert_eq!(s_page2.len(), 25);

        let all_search = storage::get_search_index(&fx.env);
        assert_eq!(all_search.len(), 125);
    });
}

