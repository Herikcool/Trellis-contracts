#![no_std]

pub mod abuse;
pub mod analytics;
pub mod auth;
pub mod batch;
pub mod canonical;
pub mod client;
pub mod compat;
pub mod config;
pub mod domain_events;
pub mod dashboard;
pub mod disclosure;
pub mod error_taxonomy;
pub mod errors;
pub mod events;
pub mod feature_flags;
pub mod health;
pub mod history;
pub mod idempotency;
pub mod impersonation;
pub mod import;
pub mod jobs;
pub mod lifecycle;
pub mod lifecycle_events;
pub mod math;
pub mod migration;
pub mod pagination;
pub mod payments;
pub mod policy;
pub mod preflight;
pub mod quota;
pub mod reconciliation;
pub mod recovery;
pub mod replay;
pub mod retention;
pub mod sanitize;
pub mod semantic;
pub mod storage;
pub mod storage_version;
pub mod telemetry;
pub mod timeline;
pub mod time_window;
pub mod utils;
pub mod webhook;

// Re-export the most commonly-needed items at crate root for ergonomic use.
pub use analytics::{
    aggregate_events, is_fully_suppressed, is_safe_dimension, is_sensitive_dimension,
    AggregateBucket, AnalyticsReport, PrivacyConfig, RawObservation, ANALYTICS_METRIC_VERSION,
};
pub use auth::{
    accept_ownership_transfer, cancel_ownership_transfer, get_admin,
    get_pending_ownership_transfer, has_permission, initialize_admin, propose_ownership_transfer,
    require_admin, require_not_paused, require_permission, role_for_permission, set_admin,
    PendingOwnershipTransfer, Permission, Role,
};
pub use batch::{
    batch_invoke_no_args, execute_multi_invoke, execute_multi_transfer, multi_transfer_all,
    BatchConfig, BatchError, BatchMode, BatchResult, BatchTransfer, OperationResult,
    ABSOLUTE_MAX_BATCH_SIZE, DEFAULT_MAX_BATCH_SIZE,
};
pub use canonical::{
    canonical_bytes, canonical_fingerprint, canonicalize_legacy, ensure_supported_encoding,
    is_legacy_encoding, normalize_int, normalize_text, parse_legacy_kv, CanonicalPart,
    CANONICAL_ENCODING_VERSION, LEGACY_ENCODING_VERSION, MAX_FIELD_LEN,
};
pub use client::{
    client_schema_fingerprint, format_client_error, AidSummaryResponse, ClientErrorResponse,
    ClientReceipt, CreateAidRequest, CreateEscrowRequest, CreateListingRequest,
    CreateProposalRequest, EscrowSummaryResponse, ListingSummaryResponse, OperationStatus,
    ProposalSummaryResponse, RebalanceRequest, RebalanceSummaryResponse, CLIENT_SCHEMA_VERSION,
};
pub use compat::{
    current_schema_version, downgrade_v2_to_v1, ensure_supported_version, from_latest,
    is_deprecated_version, is_supported_version, migrate_v1_to_v2, new_current_record, to_latest,
    CompatAidStatus, CurrentAidRecord, LegacyAidRecord, VersionedAidRecord,
    CURRENT_RECORD_SCHEMA_VERSION, MAX_SUPPORTED_RECORD_SCHEMA_VERSION,
    MIN_SUPPORTED_RECORD_SCHEMA_VERSION, RECORD_SCHEMA_V1, RECORD_SCHEMA_V2,
};
pub use config::{
    validate_feature_flag, validate_full_config, validate_network_id, validate_rpc_url,
    validate_secret_key, Environment, RedactedSecret,
};
pub use domain_events::{consumer_version, publish_event, validate_event, DomainEvent, EventSchema,
    EventValidationError};
pub use dashboard::{
    add_external_reference, create_partial_failure, generate_dashboard,
    generate_enhanced_dashboard, group_by_age, group_by_operation_type, group_by_retryability,
    group_by_severity, redact_partial_failure, DashboardReport, ExternalReference, FailureGroup,
    FailureSeverity, FailureState, HealthCategory, OperationType, PartialFailure,
    RedactedDeadLetter, RedactedPartialFailure,
};
pub use disclosure::{DetailField, DetailSeverity, TransactionDetail, TransactionDetailBuilder};
pub use error_taxonomy::{describe_error, ErrorCategory, ErrorDomain, ErrorInfo};
pub use errors::Error;
pub use events::{
    emit, emit_collection_registered, emit_import_committed, emit_import_failed,
    emit_import_simulated, emit_nft_auction, emit_nft_bid, emit_nft_listed, emit_nft_offer,
    emit_nft_settle, emit_nft_sold, emit_royalty_paid, AID_CLAIMED, AID_CREATED, AID_REFUNDED,
    AID_SETTLED, COMMISSION_PAID, CONTRACT_PAUSED, CONTRACT_RESUMED, CONTRACT_UPGRADED,
    IMPORT_COMMITTED, IMPORT_FAILED, IMPORT_SIMULATED, PARAMETER_CHANGED, PAYMENT_ESCROW_CREATED,
    PAYMENT_ESCROW_REFUNDED, PAYMENT_ESCROW_RELEASED, PAYMENT_FEE, PAYMENT_TRANSFER,
    REFERRAL_ACCRUED, REFERRAL_REGISTERED, REFERRER_SET, TIER_CONFIG_SET, TREASURY_DEPOSIT,
    TREASURY_EMERGENCY_WITHDRAW, TREASURY_SET, TREASURY_WITHDRAW,
};
pub use health::{
    get_dependency_health, list_dependency_health, set_dependency_health, DependencyHealth,
    DependencyStatus,
};
pub use impersonation::{
    check_action_permission, create_session, get_active_sessions_for_impersonator,
    get_active_sessions_for_user, get_current_impersonation, is_being_impersonated, record_action,
    revoke_session, validate_session, ImpersonationAction, ImpersonationError, ImpersonationScope,
    ImpersonationSession, ResourceScope, ResourceType, SessionParams, SessionResult, SessionState,
    DEFAULT_SESSION_DURATION, MAX_CONCURRENT_SESSIONS, MAX_SESSION_DURATION,
};
pub use import::{
    compute_batch_fingerprint, dry_run as dry_run_import, execute_import,
    generate_rollback_guidance, get_import_counter, get_imported_record, has_imported_record,
    validate_config as validate_import_config, DuplicatePolicy, ImportConfig, ImportError,
    ImportItem, ImportMode, ImportReport, RollbackGuidance, RowError, StoredImportRecord,
    ABSOLUTE_MAX_IMPORT_SIZE, DEFAULT_MAX_IMPORT_SIZE, MAX_EXTERNAL_ID_LEN,
};
pub use jobs::{
    configure_worker, dead_letter_job_ids, dedupe_key_pair, dedupe_key_u64, default_worker_config,
    discard_dead_letter, enqueue_escrow_refund, enqueue_job, get_job, get_receipt, job_stats,
    next_due_ledger, pause_worker, pending_job_ids, reprocess_job, requeue_dead_letter,
    resume_worker, run_due_job, worker_config, BackoffMode, DeadLetterRecord, EnqueueOutcome, Job,
    JobCounters, JobError, JobHandler, JobKind, JobPayload, JobReceipt, JobStats, JobStatus,
    RetryPolicy, RunOutcome, WorkerConfig, WorkerKey, ESCROW_REFUND_TAG, JOB_DEAD_LETTERED,
    JOB_ENQUEUED, JOB_REQUEUED, JOB_RETRIED, JOB_SUCCEEDED, JOB_TOPIC,
};
pub use lifecycle_events::{
    assert_lifecycle_sequence, emit_aid_event, emit_contract_record_event, emit_escrow_event,
    emit_lifecycle_transition, emit_proposal_event, emit_resource_transition, LifecycleEvent,
    LifecycleResource, LifecycleTransition, LIFECYCLE_EVENT_SCHEMA_VERSION, LIFECYCLE_TOPIC,
    STATE_NONE,
};
pub use migration::{
    begin_migration, clear_journal, dry_run, evaluate_post_checks, expected_step, fail_migration,
    finish_migration, is_resumable, load_journal, mark_step_complete, resume_index, save_journal,
    DryRunReport, MigrationError, MigrationJournal, MigrationPlan, MigrationStatus, MigrationStep,
    MigrationStepKind, PostCheck, PostCheckReport, RollbackStrategy,
};
pub use pagination::{
    paginate_id_list, paginate_id_range, Direction, PageRequest, PageResponse, DEFAULT_MAX_SCAN,
    DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT,
};
pub use payments::{
    calculate_fee, calculate_fee_split, create_escrow, deduct_fee, get_escrow, refund_escrow,
    release_escrow, safe_transfer, safe_transfer_from_contract, EscrowRecord, EscrowState,
    FeeConfig,
};
pub use preflight::{
    preflight, require_preflight, requires_preflight, PreflightCheck, PreflightCode,
    PreflightInput, PreflightOperation, PreflightReport, PreflightStatus, RiskLevel,
    EXPIRY_WARNING_WINDOW_LEDGERS, HIGH_VALUE_WARNING_BPS,
};
pub use quota::{
    check_and_consume, get_quota_config, get_quota_status, get_usage, reset_quota,
    set_quota_config, QuotaConfig, QuotaStatus, QuotaUsage,
};
pub use reconciliation::{
    run_reconciliation, DriftItem, DriftType, ReconciliationReport, SourceRecord,
};
pub use recovery::{
    abandon, complete_step, diagnostics, fail_step, is_stuck, next_action, open_operation, resume,
    OperationKind, OperationState, RecoveryCheckpoint, RecoveryDiagnostics, RecoveryError,
    RecoveryStep, StepOutcome,
};
pub use replay::{consume_payload, ReplayKey, SignedPayload};
pub use retention::{
    active_hold, apply_cleanup, class_label, classify, days_to_ledgers, default_policy,
    get_effective_policy, get_hold, get_record, get_retention_policy, is_frozen, place_hold,
    plan_cleanup, plan_cleanup_at, put_record, release_hold, set_retention_policy, CleanupEntry,
    CleanupPlan, CleanupReport, DataClass, HoldReason, RetentionHold, RetentionPolicy,
    RetentionRecord, HOLD_INDEFINITE, LEDGERS_PER_DAY, MAX_RETAIN_LEDGERS,
};
pub use sanitize::{
    sanitize_text, sanitize_url, trim_whitespace, validate_external_url, validate_safe_text,
    SafeScheme, MAX_TEXT_LEN, MAX_URL_LEN,
};
pub use semantic::{
    validate_amount, validate_distinct_parties, validate_future_expiry, AmountRule, ExpiryRule,
};
pub use storage::{
    instance_get, instance_has, instance_remove, instance_set, is_paused, persistent_extend_ttl,
    persistent_get, persistent_has, persistent_remove, persistent_set, set_paused, temporary_get,
    temporary_has, temporary_remove, temporary_set, PERSISTENT_BUMP_AMOUNT,
    PERSISTENT_TTL_THRESHOLD, TEMPORARY_BUMP_AMOUNT, TEMPORARY_TTL_THRESHOLD,
};
pub use telemetry::{
    emit_failure, emit_operation, emit_outcome, emit_success, ledger_correlation, publish,
    ActorType, TelemetryEvent, TelemetryResult, TelemetryTimer, CORE_OPERATIONS, OP_ESCROW_CREATE,
    OP_ESCROW_RELEASE, OP_PAYMENT_TRANSFER, OP_QUOTA_CONSUME, OP_REBALANCE, TELEMETRY_TOPIC,
};
pub use timeline::{
    action_audit_trail, anonymous_viewer, append_user_event, audit_trail, can_view, delete_entry,
    entry_count, entry_exists, entry_is_redacted, is_maintainer, next_audit_seq, next_seq,
    record_action_audit_event, record_audit_event, redact_entry, timeline_page, viewer_for,
    ActionAuditEntry, AuditEntry, ResourceLink, TimelineEntry, TimelineEventType, TimelineKey,
    TimelinePage, Viewer, Visibility, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, MAX_SCAN_PER_PAGE,
};
pub use utils::{is_expired, now};

#[cfg(test)]
mod test_reconciliation;

#[cfg(test)]
mod test_replay;

#[cfg(test)]
mod test_auth;
#[cfg(test)]
mod test_client;
#[cfg(test)]
mod test_disclosure;
#[cfg(test)]
mod test_import;
#[cfg(test)]
mod test_jobs;
#[cfg(test)]
mod test_pagination;
#[cfg(test)]
mod test_sanitize;
#[cfg(test)]
mod test_storage;
#[cfg(test)]
mod test_timeline;

// Deterministic ledger-sequence harness controls (Issue #156). Test-only: the
// harness sets the ledger sequence/timestamp explicitly and never reads the
// host clock, so boundary tests are reproducible.
#[cfg(test)]
pub mod ledger_sequence;

#[cfg(test)]
mod test_ledger_sequence;
