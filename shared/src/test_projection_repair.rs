use soroban_sdk::{symbol_short, Env, Vec};
use crate::projection_repair::{
    default_monitor_config, detect_cache_drift, detect_index_drift,
    execute_or_queue_projection_repair, get_latest_audit_receipt, get_pending_queue_count,
    process_queued_repair, recommend_repair, rollback_projection_repair, set_monitor_config,
    ProjectionKind, ProjectionModule, ProjectionRepairError, ProjectionScope,
    ProjectionStalenessSeverity, RepairActionOutcome, RepairRecommendation, StaleDriftKind,
    StaleProjectionItem,
};

#[test]
fn test_treasury_projections_and_quarantine() {
    let env = Env::default();

    // 1. Strategy Index drift
    let strat_scope = ProjectionScope {
        module: ProjectionModule::Treasury,
        kind: ProjectionKind::Index,
        projection_id: symbol_short!("strat_idx"),
        sub_id: 0,
    };

    let mut canonical_strats = Vec::new(&env);
    canonical_strats.push_back(1u64); // Strategy A
    canonical_strats.push_back(2u64); // Strategy B
    canonical_strats.push_back(3u64); // Strategy C

    let mut projected_strats = Vec::new(&env);
    projected_strats.push_back(1u64);
    projected_strats.push_back(2u64);
    // Missing Strategy 3

    let strat_findings = detect_index_drift(
        &env,
        &strat_scope,
        &canonical_strats,
        &projected_strats,
        150,
        140,
        50,
    );
    assert_eq!(strat_findings.len(), 1);
    let finding = strat_findings.get(0).unwrap();
    assert_eq!(finding.drift_kind, StaleDriftKind::MissingIndexEntry);
    assert_eq!(finding.affected_id, 3);

    // 2. Financial category balance drift -> Must trip quarantine
    let bal_scope = ProjectionScope {
        module: ProjectionModule::Treasury,
        kind: ProjectionKind::Cache,
        projection_id: symbol_short!("cat_bal"),
        sub_id: 10, // Emergency relief category
    };

    let bal_drift = detect_cache_drift(
        &env,
        &bal_scope,
        10_000_000,
        9_200_000,
        150,
        120,
        20,
        true, // is_financial_balance
    );
    assert!(bal_drift.is_some());
    let bal_item = bal_drift.unwrap();
    assert_eq!(bal_item.severity, ProjectionStalenessSeverity::Critical);

    let mut items = Vec::new(&env);
    items.push_back(finding);
    items.push_back(bal_item);

    let receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Treasury,
        &items,
        false,
    )
    .unwrap();

    assert_eq!(receipt.total_scanned, 2);
    assert_eq!(receipt.repaired_count, 1); // Strategy index repaired immediately
    assert_eq!(receipt.quarantined_count, 1); // Category balance quarantined
    assert!(!receipt.clean); // Not clean because quarantined
}

#[test]
fn test_aid_projections_search_and_donor_indexes() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Aid,
        kind: ProjectionKind::Index,
        projection_id: symbol_short!("search"),
        sub_id: 0,
    };

    let mut canonical_aids = Vec::new(&env);
    canonical_aids.push_back(201u64);
    canonical_aids.push_back(202u64);
    canonical_aids.push_back(203u64);

    let mut projected_aids = Vec::new(&env);
    projected_aids.push_back(201u64);
    projected_aids.push_back(204u64); // Orphaned aid record in search

    let findings = detect_index_drift(
        &env,
        &scope,
        &canonical_aids,
        &projected_aids,
        300,
        290,
        50,
    );
    assert_eq!(findings.len(), 3); // 2 missing (202, 203), 1 orphaned (204)

    let receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Aid,
        &findings,
        false,
    )
    .unwrap();

    assert_eq!(receipt.repaired_count, 3);
    assert_eq!(receipt.quarantined_count, 0);
    assert!(receipt.clean);
    assert!(receipt.post_repair_fingerprint > 0);
}

#[test]
fn test_referral_projections_downline_and_commission() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Referral,
        kind: ProjectionKind::Cache,
        projection_id: symbol_short!("comm_acc"),
        sub_id: 777, // Referrer user ID
    };

    let drift = detect_cache_drift(
        &env,
        &scope,
        3_500,
        3_100,
        500,
        480,
        50,
        false, // Accrued rewards projection
    );
    assert!(drift.is_some());
    let item = drift.unwrap();

    let mut items = Vec::new(&env);
    items.push_back(item);

    let receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Referral,
        &items,
        false,
    )
    .unwrap();

    assert_eq!(receipt.repaired_count, 1);
    let action = receipt.actions.get(0).unwrap();
    assert_eq!(action.prior_projected_value, 3_100);
    assert_eq!(action.new_projected_value, 3_500);
}

#[test]
fn test_governance_projections_active_proposals_and_vote_tally() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Governance,
        kind: ProjectionKind::Cache,
        projection_id: symbol_short!("tally"),
        sub_id: 42, // Proposal ID
    };

    let drift = detect_cache_drift(
        &env,
        &scope,
        150, // Canonical sum of cast ballots
        145, // Cached tally
        600,
        595,
        50,
        false,
    );
    assert!(drift.is_some());

    let mut items = Vec::new(&env);
    items.push_back(drift.unwrap());

    let receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Governance,
        &items,
        false,
    )
    .unwrap();

    assert_eq!(receipt.repaired_count, 1);
    assert!(receipt.clean);
}

#[test]
fn test_oracle_projections_price_staleness_and_round_cache() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Oracle,
        kind: ProjectionKind::Cache,
        projection_id: symbol_short!("price_rnd"),
        sub_id: 1, // XLM/USD feed
    };

    // Stale price round timestamp lag (lag = 80 ledgers > tolerance 50)
    let drift = detect_cache_drift(
        &env,
        &scope,
        500_000,
        500_000, // Value matches, but timestamp is lagging
        1_000,
        920,
        50,
        false,
    );
    assert!(drift.is_some());
    let item = drift.unwrap();
    assert_eq!(item.drift_kind, StaleDriftKind::LagsCanonicalLedger);
    assert_eq!(item.lag_ledgers, 80);
}

#[test]
fn test_marketplace_projections_collection_listings_and_floor() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Marketplace,
        kind: ProjectionKind::Index,
        projection_id: symbol_short!("listing"),
        sub_id: 99, // Collection ID
    };

    let mut canonical_tokens = Vec::new(&env);
    canonical_tokens.push_back(10u64);
    canonical_tokens.push_back(20u64);

    let mut projected_tokens = Vec::new(&env);
    projected_tokens.push_back(10u64);
    // Missing listing 20

    let findings = detect_index_drift(
        &env,
        &scope,
        &canonical_tokens,
        &projected_tokens,
        800,
        790,
        50,
    );
    assert_eq!(findings.len(), 1);

    let receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Marketplace,
        &findings,
        false,
    )
    .unwrap();

    assert_eq!(receipt.repaired_count, 1);
    assert_eq!(receipt.module, ProjectionModule::Marketplace);
}

#[test]
fn test_batch_queuing_and_worker_drain() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Aid,
        kind: ProjectionKind::Index,
        projection_id: symbol_short!("search"),
        sub_id: 0,
    };

    // Configure low immediate repair threshold of 2 items
    let mut config = default_monitor_config(&env);
    config.max_immediate_repairs = 2;
    set_monitor_config(&env, ProjectionModule::Aid, &config);

    let mut items = Vec::new(&env);
    let mut i = 0u64;
    while i < 5 {
        items.push_back(StaleProjectionItem {
            scope: scope.clone(),
            drift_kind: StaleDriftKind::MissingIndexEntry,
            severity: ProjectionStalenessSeverity::Medium,
            affected_id: 1000 + i,
            canonical_value: 1,
            projected_value: 0,
            lag_ledgers: 5,
            detail: symbol_short!("queued_it"),
        });
        i += 1;
    }

    let receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Aid,
        &items,
        false,
    )
    .unwrap();

    // With 5 items and limit 2, all 5 are queued for background worker
    assert_eq!(receipt.queued_count, 5);
    assert_eq!(get_pending_queue_count(&env, ProjectionModule::Aid), 5);

    // Background worker drains queue
    let action_0 = process_queued_repair(&env, ProjectionModule::Aid, 0).unwrap();
    assert_eq!(action_0.outcome, RepairActionOutcome::RepairedImmediately);

    let action_1 = process_queued_repair(&env, ProjectionModule::Aid, 1).unwrap();
    assert_eq!(action_1.outcome, RepairActionOutcome::RepairedImmediately);
}

#[test]
fn test_repair_rollback_and_evidence_preservation() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Referral,
        kind: ProjectionKind::Cache,
        projection_id: symbol_short!("acc_comm"),
        sub_id: 123,
    };

    let mut items = Vec::new(&env);
    items.push_back(StaleProjectionItem {
        scope,
        drift_kind: StaleDriftKind::StaleCounterMismatch,
        severity: ProjectionStalenessSeverity::Medium,
        affected_id: 123,
        canonical_value: 200,
        projected_value: 150,
        lag_ledgers: 5,
        detail: symbol_short!("mismatch"),
    });

    let exec_receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Referral,
        &items,
        false,
    )
    .unwrap();
    assert_eq!(exec_receipt.repaired_count, 1);

    // Rollback repair
    let rollback_receipt = rollback_projection_repair(&env, exec_receipt.receipt_id).unwrap();
    assert_eq!(rollback_receipt.repaired_count, 1);
    assert_eq!(rollback_receipt.actions.get(0).unwrap().new_projected_value, 150);

    // Invariant: Rollback snapshot was cleared, so second rollback fails
    let double_rollback_err = rollback_projection_repair(&env, exec_receipt.receipt_id).unwrap_err();
    assert_eq!(double_rollback_err, ProjectionRepairError::RollbackNotFound);
}

#[test]
fn test_dry_run_guarantees() {
    let env = Env::default();
    let scope = ProjectionScope {
        module: ProjectionModule::Oracle,
        kind: ProjectionKind::Cache,
        projection_id: symbol_short!("feed"),
        sub_id: 2,
    };

    let mut items = Vec::new(&env);
    items.push_back(StaleProjectionItem {
        scope,
        drift_kind: StaleDriftKind::StaleCounterMismatch,
        severity: ProjectionStalenessSeverity::Low,
        affected_id: 2,
        canonical_value: 10,
        projected_value: 5,
        lag_ledgers: 1,
        detail: symbol_short!("dry_test"),
    });

    let receipt = execute_or_queue_projection_repair(
        &env,
        ProjectionModule::Oracle,
        &items,
        true, // dry_run
    )
    .unwrap();

    assert_eq!(receipt.receipt_id, 0);
    assert_eq!(receipt.total_scanned, 1);
    assert!(get_latest_audit_receipt(&env, ProjectionModule::Oracle).is_none());
}
