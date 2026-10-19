#![cfg(test)]

//! Tamper-detection and export tests for [`crate::audit_ledger`] (Issue #233).
//!
//! Every test runs inside a registered contract context because Soroban only
//! permits storage access from a contract frame. The harness is intentionally
//! minimal: it exists only to give the ledger a storage namespace.

extern crate std;

use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{Address as _, Events as _, Ledger as _},
    Address, BytesN, Env, Symbol, TryFromVal,
};

use crate::audit_ledger::{
    append_event, append_financial, append_privileged, checkpoint, entry_hash, export_digest,
    export_ledger, genesis_hash, get_entry, head, length, publish_head_checkpoint, verify_chain,
    verify_export, AuditEventKind, AuditEventParams, AuditLedgerEntry, AuditLedgerKey,
    AuditVerificationFailure, AUDIT_LEDGER_SCHEMA_VERSION, MAX_EXPORT_LIMIT,
};
use crate::auth;
use crate::errors::Error;
use crate::storage::{instance_set, persistent_get, persistent_remove, persistent_set};

#[contract]
pub struct AuditLedgerHarness;

#[contractimpl]
impl AuditLedgerHarness {
    pub fn noop(_env: Env) {}
}

fn register(env: &Env) -> Address {
    env.register_contract(None, AuditLedgerHarness)
}

fn with_ledger<T>(env: &Env, f: impl FnOnce() -> T) -> T {
    let id = register(env);
    env.as_contract(&id, f)
}

/// Runs one export in its own contract frame. Each `require_auth` for the same
/// address inside a single invocation is rejected by recording-auth mode, so
/// tests must not export twice in one frame.
fn export_at(
    env: &Env,
    id: &Address,
    admin: &Address,
    from_seq: u64,
    limit: u32,
) -> Result<crate::audit_ledger::AuditLedgerExport, Error> {
    env.as_contract(id, || export_ledger(env, admin, from_seq, limit))
}

fn params(env: &Env, kind: AuditEventKind, actor: &Address) -> AuditEventParams {
    AuditEventParams {
        kind,
        module: symbol_short!("treasury"),
        actor: actor.clone(),
        subject: None,
        resource: None,
        attribute: Some(symbol_short!("reserve")),
        amount: Some(500),
        previous_amount: Some(100),
        note: Some(symbol_short!("aud_test")),
        correlation_id: genesis_hash(env),
    }
}

fn append_at(
    env: &Env,
    ledger: u32,
    timestamp: u64,
    kind: AuditEventKind,
    actor: &Address,
) -> AuditLedgerEntry {
    env.ledger().set_sequence_number(ledger);
    env.ledger().set_timestamp(timestamp);
    append_event(env, params(env, kind, actor)).unwrap()
}

// ---------------------------------------------------------------------------
// Chain construction
// ---------------------------------------------------------------------------

#[test]
fn honest_chain_links_every_entry_and_verifies() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        let first = append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        let second = append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);
        let third = append_at(&env, 12, 1_200, AuditEventKind::RoleGranted, &actor);

        assert_eq!(first.seq, 1);
        assert_eq!(first.prev_hash, genesis_hash(&env));
        assert_eq!(second.prev_hash, first.hash);
        assert_eq!(third.prev_hash, second.hash);
        assert_eq!(length(&env), 3);
        assert_eq!(entry_hash(&env, &first), first.hash);

        let report = verify_chain(&env);
        assert!(report.verified);
        assert_eq!(report.failure, AuditVerificationFailure::None);
        assert_eq!(report.first_invalid_seq, 0);
        assert_eq!(report.entries_checked, 3);
        assert_eq!(report.head_hash, third.hash);
        assert_eq!(checkpoint(&env).head_hash, third.hash);
        assert_eq!(head(&env).schema_version, AUDIT_LEDGER_SCHEMA_VERSION);
    });
}

#[test]
fn same_event_at_a_different_position_has_a_different_hash() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        let first = append_at(&env, 10, 1_000, AuditEventKind::CommissionPaid, &actor);
        let second = append_at(&env, 10, 1_000, AuditEventKind::CommissionPaid, &actor);

        // Identical event payloads are still distinguishable because the
        // sequence number and previous hash are part of the digest.
        assert_ne!(first.hash, second.hash);
        assert_ne!(first.seq, second.seq);
        assert!(verify_chain(&env).verified);
    });
}

#[test]
fn event_kind_selects_the_privileged_or_financial_class() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        let privileged = append_at(&env, 10, 1_000, AuditEventKind::ContractPaused, &actor);
        let financial = append_at(&env, 11, 1_100, AuditEventKind::AidClaimed, &actor);

        assert_eq!(
            privileged.class,
            crate::audit_ledger::AuditEventClass::Privileged
        );
        assert_eq!(
            financial.class,
            crate::audit_ledger::AuditEventClass::Financial
        );
    });
}

#[test]
fn wrappers_reject_a_kind_from_the_other_class() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        assert_eq!(
            append_privileged(
                &env,
                AuditEventKind::TreasuryDeposit,
                symbol_short!("treasury"),
                actor.clone(),
                None,
                None,
                None,
            ),
            Err(Error::InvalidArgument)
        );
        assert_eq!(
            append_financial(
                &env,
                AuditEventKind::RoleGranted,
                symbol_short!("treasury"),
                actor.clone(),
                None,
                None,
                None,
                None,
                None,
            ),
            Err(Error::InvalidArgument)
        );
    });
}

// ---------------------------------------------------------------------------
// Chronology
// ---------------------------------------------------------------------------

#[test]
fn append_rejects_a_ledger_sequence_regression() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 20, 2_000, AuditEventKind::TreasuryDeposit, &actor);

        env.ledger().set_sequence_number(19);
        env.ledger().set_timestamp(2_100);
        assert_eq!(
            append_event(
                &env,
                params(&env, AuditEventKind::TreasuryWithdrawal, &actor)
            ),
            Err(Error::InvalidArgument)
        );

        // The rejected append wrote nothing.
        assert_eq!(length(&env), 1);
        assert!(verify_chain(&env).verified);
    });
}

#[test]
fn append_rejects_a_timestamp_regression_within_the_same_ledger() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 20, 2_000, AuditEventKind::TreasuryDeposit, &actor);

        env.ledger().set_sequence_number(20);
        env.ledger().set_timestamp(1_999);
        assert_eq!(
            append_event(
                &env,
                params(&env, AuditEventKind::TreasuryWithdrawal, &actor)
            ),
            Err(Error::InvalidArgument)
        );
        assert_eq!(length(&env), 1);
    });
}

#[test]
fn equal_ledger_and_timestamp_is_accepted() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 20, 2_000, AuditEventKind::TreasuryDeposit, &actor);

        env.ledger().set_sequence_number(20);
        env.ledger().set_timestamp(2_000);
        let second = append_event(
            &env,
            params(&env, AuditEventKind::TreasuryWithdrawal, &actor),
        )
        .unwrap();
        assert_eq!(second.seq, 2);
        assert!(verify_chain(&env).verified);
    });
}

// ---------------------------------------------------------------------------
// Tamper detection
// ---------------------------------------------------------------------------

#[test]
fn editing_stored_amount_is_detected_at_the_edited_entry() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);
        append_at(&env, 12, 1_200, AuditEventKind::TreasuryDeposit, &actor);

        let mut tampered: AuditLedgerEntry =
            persistent_get(&env, &AuditLedgerKey::Entry(2)).unwrap();
        tampered.amount = Some(999_999);
        persistent_set(&env, &AuditLedgerKey::Entry(2), &tampered);

        let report = verify_chain(&env);
        assert!(!report.verified);
        assert_eq!(report.failure, AuditVerificationFailure::HashMismatch);
        assert_eq!(report.first_invalid_seq, 2);
    });
}

#[test]
fn editing_metadata_is_detected() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::RoleGranted, &actor);

        let mut tampered: AuditLedgerEntry =
            persistent_get(&env, &AuditLedgerKey::Entry(1)).unwrap();
        tampered.note = Some(symbol_short!("forged"));
        persistent_set(&env, &AuditLedgerKey::Entry(1), &tampered);

        let report = verify_chain(&env);
        assert_eq!(report.failure, AuditVerificationFailure::HashMismatch);
        assert_eq!(report.first_invalid_seq, 1);
    });
}

#[test]
fn rehashing_a_tampered_entry_still_breaks_the_chain() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);

        // An attacker who knows the hash function can recompute entry 1's hash
        // after editing it. The next entry still commits to the original hash,
        // so verification fails at the link instead of the local hash.
        let mut tampered: AuditLedgerEntry =
            persistent_get(&env, &AuditLedgerKey::Entry(1)).unwrap();
        tampered.amount = Some(1);
        tampered.hash = entry_hash(&env, &tampered);
        persistent_set(&env, &AuditLedgerKey::Entry(1), &tampered);

        let report = verify_chain(&env);
        assert!(!report.verified);
        assert_eq!(report.failure, AuditVerificationFailure::BrokenLink);
        assert_eq!(report.first_invalid_seq, 2);
    });
}

#[test]
fn deleting_an_entry_is_detected() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);
        append_at(&env, 12, 1_200, AuditEventKind::AidCreated, &actor);

        persistent_remove(&env, &AuditLedgerKey::Entry(2));

        let report = verify_chain(&env);
        assert_eq!(report.failure, AuditVerificationFailure::MissingEntry);
        assert_eq!(report.first_invalid_seq, 2);
        assert_eq!(report.entries_checked, 1);
    });
}

#[test]
fn rolling_back_the_head_is_detected_as_an_unexpected_tail() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);

        let mut current = head(&env);
        current.length = 1;
        current.head_hash = get_entry(&env, 1).unwrap().hash;
        current.last_ledger = 10;
        current.last_timestamp = 1_000;
        instance_set(&env, &AuditLedgerKey::Head, &current);

        let report = verify_chain(&env);
        assert_eq!(report.failure, AuditVerificationFailure::UnexpectedTail);
        assert_eq!(report.first_invalid_seq, 2);
    });
}

#[test]
fn editing_the_head_hash_is_detected() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);

        let mut current = head(&env);
        current.head_hash = BytesN::from_array(&env, &[7u8; 32]);
        instance_set(&env, &AuditLedgerKey::Head, &current);

        let report = verify_chain(&env);
        assert_eq!(report.failure, AuditVerificationFailure::HeadMismatch);
        assert_eq!(report.first_invalid_seq, 1);
    });
}

#[test]
fn swapping_entries_is_detected_as_non_monotonic() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);

        let first: AuditLedgerEntry = persistent_get(&env, &AuditLedgerKey::Entry(1)).unwrap();
        let second: AuditLedgerEntry = persistent_get(&env, &AuditLedgerKey::Entry(2)).unwrap();
        persistent_set(&env, &AuditLedgerKey::Entry(1), &second);
        persistent_set(&env, &AuditLedgerKey::Entry(2), &first);

        let report = verify_chain(&env);
        assert_eq!(
            report.failure,
            AuditVerificationFailure::NonMonotonicSequence
        );
        assert_eq!(report.first_invalid_seq, 1);
    });
}

#[test]
fn changing_an_entry_ledger_out_of_order_is_detected() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 2_000, AuditEventKind::TreasuryWithdrawal, &actor);

        // Rewrite entry 2 with an older ledger *and* rehash it, so only the
        // chronology rule can catch it.
        let mut tampered: AuditLedgerEntry =
            persistent_get(&env, &AuditLedgerKey::Entry(2)).unwrap();
        tampered.ledger = 9;
        tampered.timestamp = 500;
        tampered.hash = entry_hash(&env, &tampered);
        persistent_set(&env, &AuditLedgerKey::Entry(2), &tampered);
        let mut current = head(&env);
        current.head_hash = tampered.hash;
        instance_set(&env, &AuditLedgerKey::Head, &current);

        let report = verify_chain(&env);
        assert_eq!(report.failure, AuditVerificationFailure::LedgerRegression);
        assert_eq!(report.first_invalid_seq, 2);
    });
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

#[test]
fn export_requires_audit_read_permission() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let actor = Address::generate(&env);
    let stranger = Address::generate(&env);

    let id = register(&env);
    // Initialise in its own frame: a second `require_auth` for the same
    // address inside one invocation is rejected by the recording auth mode.
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());
    env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        assert_eq!(
            export_ledger(&env, &stranger, 1, 10),
            Err(Error::Unauthorized)
        );
        assert!(export_ledger(&env, &admin, 1, 10).is_ok());
    });
}

#[test]
fn exported_pages_are_bounded_verifiable_and_cursorable() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let actor = Address::generate(&env);

    let id = register(&env);
    // Initialise in its own frame: a second `require_auth` for the same
    // address inside one invocation is rejected by the recording auth mode.
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());
    env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);
        append_at(&env, 12, 1_200, AuditEventKind::AidCreated, &actor);
    });

    let page = export_at(&env, &id, &admin, 1, 2).unwrap();
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.from_seq, 1);
    assert_eq!(page.to_seq, 2);
    assert!(page.has_more);
    assert_eq!(page.start_prev_hash, genesis_hash(&env));
    assert!(verify_export(&env, &page).verified);

    let rest = export_at(&env, &id, &admin, 3, 10).unwrap();
    assert_eq!(rest.entries.len(), 1);
    assert!(!rest.has_more);
    assert_eq!(rest.ledger_length, 3);
    let report = verify_export(&env, &rest);
    assert!(report.verified);
    assert_eq!(report.entries_checked, 3);

    // Oversized requests are capped, never unbounded.
    let capped = export_at(&env, &id, &admin, 1, u32::MAX).unwrap();
    assert_eq!(capped.entries.len(), 3);
    assert!(MAX_EXPORT_LIMIT < u32::MAX);
}

#[test]
fn empty_tail_export_anchors_at_the_head_and_verifies() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let actor = Address::generate(&env);

    let id = register(&env);
    // Initialise in its own frame: a second `require_auth` for the same
    // address inside one invocation is rejected by the recording auth mode.
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());
    let head_hash = env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        head(&env).head_hash
    });

    let tail = export_at(&env, &id, &admin, 2, 10).unwrap();
    assert_eq!(tail.from_seq, 2);
    assert_eq!(tail.to_seq, 1);
    assert!(tail.entries.is_empty());
    assert!(!tail.has_more);
    assert_eq!(tail.start_prev_hash, head_hash);
    assert!(verify_export(&env, &tail).verified);
}

#[test]
fn tampered_export_entry_is_detected_by_digest() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let actor = Address::generate(&env);

    let id = register(&env);
    // Initialise in its own frame: a second `require_auth` for the same
    // address inside one invocation is rejected by the recording auth mode.
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());
    env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        append_at(&env, 11, 1_100, AuditEventKind::TreasuryWithdrawal, &actor);

        let mut bundle = export_ledger(&env, &admin, 1, 10).unwrap();
        let mut entry = bundle.entries.get(0).unwrap();
        entry.amount = Some(1);
        bundle.entries.set(0, entry);

        let report = verify_export(&env, &bundle);
        assert_eq!(
            report.failure,
            AuditVerificationFailure::ExportDigestMismatch
        );
    });
}

#[test]
fn tampered_export_entry_with_recomputed_digest_fails_hash_check() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let actor = Address::generate(&env);

    let id = register(&env);
    // Initialise in its own frame: a second `require_auth` for the same
    // address inside one invocation is rejected by the recording auth mode.
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());
    env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);

        let mut bundle = export_ledger(&env, &admin, 1, 10).unwrap();
        let mut entry = bundle.entries.get(0).unwrap();
        entry.amount = Some(1);
        bundle.entries.set(0, entry);
        // Recompute the digest so the bundle is internally self-consistent
        // apart from the edited entry hash.
        bundle.digest = export_digest(&env, &bundle);

        let report = verify_export(&env, &bundle);
        assert!(!report.verified);
        assert_eq!(report.failure, AuditVerificationFailure::HashMismatch);
        assert_eq!(report.first_invalid_seq, 1);
    });
}

#[test]
fn export_with_a_forged_head_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let actor = Address::generate(&env);

    let id = register(&env);
    // Initialise in its own frame: a second `require_auth` for the same
    // address inside one invocation is rejected by the recording auth mode.
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());
    env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);

        let mut bundle = export_ledger(&env, &admin, 1, 10).unwrap();
        bundle.head_hash = BytesN::from_array(&env, &[9u8; 32]);
        bundle.digest = export_digest(&env, &bundle);

        let report = verify_export(&env, &bundle);
        assert_eq!(report.failure, AuditVerificationFailure::HeadMismatch);
    });
}

#[test]
fn export_out_of_range_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let actor = Address::generate(&env);

    let id = register(&env);
    // Initialise in its own frame: a second `require_auth` for the same
    // address inside one invocation is rejected by the recording auth mode.
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());
    env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
    });

    assert_eq!(
        export_at(&env, &id, &admin, 0, 10),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        export_at(&env, &id, &admin, 3, 10),
        Err(Error::InvalidArgument)
    );
}

// ---------------------------------------------------------------------------
// Durability and anchoring
// ---------------------------------------------------------------------------

#[test]
fn chain_survives_reloading_the_contract_storage() {
    let env = Env::default();
    let actor = Address::generate(&env);
    let id = register(&env);

    env.as_contract(&id, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
    });

    // A separate contract frame re-reads the persisted entry and head.
    env.as_contract(&id, || {
        assert_eq!(length(&env), 1);
        assert!(get_entry(&env, 1).is_some());
        assert!(verify_chain(&env).verified);
    });
}

#[test]
fn head_checkpoint_event_carries_the_current_anchor() {
    let env = Env::default();
    let actor = Address::generate(&env);

    with_ledger(&env, || {
        append_at(&env, 10, 1_000, AuditEventKind::TreasuryDeposit, &actor);
        let checkpoint = publish_head_checkpoint(&env);

        let events = env.events().all();
        let (_, topics, _) = events.last().unwrap();
        assert_eq!(
            Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
            symbol_short!("audit")
        );
        assert_eq!(
            Symbol::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
            symbol_short!("head")
        );
        assert_eq!(checkpoint.length, 1);
        assert_eq!(checkpoint.head_hash, get_entry(&env, 1).unwrap().hash);
    });
}

#[test]
fn verifying_an_empty_ledger_succeeds() {
    let env = Env::default();
    with_ledger(&env, || {
        let report = verify_chain(&env);
        assert!(report.verified);
        assert_eq!(report.entries_checked, 0);
        assert_eq!(report.head_hash, genesis_hash(&env));
    });
}

#[test]
fn exported_bundle_for_an_empty_ledger_verifies() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = register(&env);
    env.as_contract(&id, || auth::initialize_admin(&env, &admin).unwrap());

    env.as_contract(&id, || {
        let bundle = export_ledger(&env, &admin, 1, 5).unwrap();
        assert!(bundle.entries.is_empty());
        assert_eq!(bundle.from_seq, 1);
        assert_eq!(bundle.to_seq, 0);
        assert_eq!(bundle.ledger_length, 0);
        assert!(verify_export(&env, &bundle).verified);
    });
}
