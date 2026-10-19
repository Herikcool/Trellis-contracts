# Operational Metadata: Data Minimization Review and Retention Enforcement (Issue #229)

Operational metadata — audit trails, telemetry, execution checkpoints, support evidence, settlement records, and dispute data — is essential for system reliability and observability across Trellis smart contracts. However, on-chain operational metadata must not accumulate indefinitely or store unneeded, sensitive data.

This specification documents the **data minimization review framework** and **retention enforcement mechanisms** implemented across the six core Soroban contract modules:
1. **Treasury** (`contracts/treasury-contract`)
2. **Aid** (`contracts/aid-contract`)
3. **Referral** (`contracts/referral-contract`)
4. **Governance** (`contracts/governance-contract`)
5. **Oracle** (`contracts/oracle-contract`)
6. **Marketplace** (`contracts/nft-marketplace`)

The implementation lives in [`shared/src/operational_metadata.rs`](../shared/src/operational_metadata.rs) and is re-exported from the `shared` crate root.

---

## 1. Data Classification Taxonomy

Operational metadata is classified in two orthogonal dimensions: **Data Class** (lifecycle & retention window) and **Field Sensitivity** (necessity & privacy level).

### 1.1 Data Classes

| `MetadataClass` | Symbol | Default Window | Description | Deletion Frozen |
|---|---|---|---|---|
| `Audit` | `audit` | 365 days | Privileged/administrative audit trails | No |
| `Telemetry` | `telemetry` | 30 days (14 days for Oracle) | Ephemeral metrics, gas/budget samples, status health | No |
| `Execution` | `exec` | 90 days | Batch execution progress, intermediate checkpoints | No |
| `SupportEvidence` | `support` | 180 days | Support incident captures and proof hashes | No |
| `FinancialSettlement` | `settle` | 2,555 days (~7 years) | Escrow payouts, fee splits, obligation settlement | **Yes** |
| `Dispute` | `dispute` | 365 days | Slashed deposits, contested claims, active litigation | **Yes** |

> [!NOTE]
> Retention windows are expressed in ledgers assuming the network's ~5 s/ledger cadence (`LEDGERS_PER_DAY = 17_280`).
> Hard ceiling on any custom window is 10 years (`MAX_RETAIN_LEDGERS = 63_072_000`).

### 1.2 Field Sensitivity

| Sensitivity Level | Action | Description |
|---|---|---|
| `Essential` | **Retain** | Mandatory for state verification, cryptographic proofs, or asset conservation invariants (e.g. `actor`, `action`, `token`, `amount`, `seq`, `aid_id`). |
| `OptionalContext` | **Retain if safe** | Helpful operational context that does not leak private details and stays within size budgets. |
| `Redundant` | **Strip** | Unnecessary or duplicated fields (e.g. wall-clock timestamps when ledger sequence is present, duplicate display titles, verbose UI themes). |
| `Prohibited` | **Strictly Reject/Purge** | Highly sensitive data (e.g. raw PII, contact info, credentials, API secrets, unhashed private notes, raw debug stack traces). |

---

## 2. Module-Specific Minimization Rules

The minimization review engine applies per-module classification before storing metadata:

### 2.1 Treasury Module
- **Essential**: `actor`, `action`, `token`, `amount`, `category`, `seq`
- **Redundant**: `wall_time`, `token_name`, `memo_str`, `client_ts`
- **Prohibited**: `priv_key`, `secret`, `token_sec`, `raw_memo`, `ip_addr`

### 2.2 Aid Module
- **Essential**: `aid_id`, `donor`, `claimant`, `amount`, `status`
- **Redundant**: `disp_name`, `org_title`, `wall_time`, `client_ver`
- **Prohibited**: `biometric`, `gps_loc`, `user_pii`, `phone`, `email`

### 2.3 Referral Module
- **Essential**: `ref_code`, `referrer`, `referee`, `tier`, `commiss`
- **Redundant**: `campaign`, `vanity_dup`, `wall_time`
- **Prohibited**: `contact_info`, `secret`, `api_token`

### 2.4 Governance Module
- **Essential**: `prop_id`, `action`, `proposer`, `approver`, `quorum`
- **Redundant**: `prose_desc`, `client_id`, `wall_time`
- **Prohibited**: `ballot_sec`, `voter_sig`, `priv_key`

### 2.5 Oracle Module
- **Essential**: `feed_id`, `submitter`, `price`, `round`, `tx_hash`
- **Redundant**: `prov_tag`, `raw_resp`, `peer_list`
- **Prohibited**: `api_token`, `secret`, `dbg_trace`

### 2.6 Marketplace Module
- **Essential**: `item_id`, `seller`, `bidder`, `price`, `royalty`
- **Redundant**: `item_desc`, `view_mode`, `ui_theme`
- **Prohibited**: `card_no`, `user_pii`, `email`, `phone`

---

## 3. Data Minimization Review Workflow

Before metadata is persisted, it passes through `review_and_minimize`:

```rust
let (review_result, stored_record) = review_and_minimize(
    &env,
    &record_id,
    OperationalModule::Aid,
    MetadataClass::Audit,
    &raw_fields,
    created_ledger,
)?;

assert!(review_result.passed);
assert_eq!(review_result.stripped_fields.len(), 2); // Redundant and prohibited stripped
store_operational_metadata(&env, &stored_record)?;
```

1. Each field is inspected against global and module-specific rules.
2. `Redundant` and `Prohibited` fields are stripped and logged in `MinimizationReviewResult::stripped_fields`.
3. `Essential` and non-redundant `OptionalContext` fields are packed into `StoredOperationalMetadata::fields`.
4. The exact on-chain footprint (`footprint_bytes`) is calculated and stored with the record.

---

## 4. Retention Enforcement & Testable Deletion

Deletion follows the Trellis **report-first** pattern:

### 4.1 Step 1: Generate Cleanup Plan (Dry Run)

```rust
let plan = plan_operational_cleanup(&env, &records);
// or evaluated at a fixed ledger sequence:
let plan = plan_operational_cleanup_at(&env, &records, target_ledger);

// plan.eligible          -> Expired, unprotected records (reclaimable)
// plan.protected_held    -> Expired, but protected by active hold
// plan.protected_frozen  -> Settlement/dispute classes (never auto-deleted)
// plan.retained_active   -> Still within retention window
// plan.bytes_reclaimable -> Exact bytes freed if eligible records are purged
```

- Guarantees `plan.dry_run == true`. A dry-run plan **never mutates storage**.

### 4.2 Step 2: Review and Execute Deletion

```rust
let mut live_plan = plan.clone();
live_plan.dry_run = false;

// Requires explicit confirm == true
let report = apply_operational_cleanup(&env, &live_plan, true)?;
// report.deleted            -> Record IDs removed from persistent storage
// report.skipped_protected  -> Protected IDs preserved
// report.bytes_reclaimed    -> Bytes reclaimed
```

### 4.3 Protection Guarantees

1. **Active Holds (`OperationalHold`)**:
   - `place_operational_hold(env, id, reason, until_ledger)` protects an expired record from deletion.
   - Indefinite holds (`until_ledger == HOLD_INDEFINITIE`) never lapse until explicitly released via `release_operational_hold`.
2. **Late Holds**:
   - Holds placed *after* the cleanup plan was produced are re-checked during execution. The late hold wins and is recorded in `report.skipped_protected`.
3. **Frozen Classes**:
   - `FinancialSettlement` and `Dispute` metadata cannot be deleted by `apply_operational_cleanup`, even if an operator manually inserts their IDS into `plan.eligible`.

---

## 5. Operational Edge Cases & Failure Modes

### 5.1 Edge Cases

| Scenario | Behavior | Handling |
|---|---|---|
| **Late Hold Collision** | Hold placed between plan creation and confirmed execution. | Execution re-checks active holds in real-time. Record is skipped and reported in `report.skipped_protected`. |
| **Clock / Sequence Boundary** | Evaluated at the exact expiration ledger (`sequence == expires_ledger`). | Considered expired; eligible for deletion unless under active hold. |
| **Lapsed Hold** | Record had a hold with `until_ledger <= current_ledger`. | Hold is ignored as lapsed; record is eligible for deletion if retention window expired. |
| **Corrupted / Empty Created Ledger** | Metadata submitted with `created_ledger == 0`. | Rejected with `Error::InvalidArgument`. |
| **Zero Retention Window Override** | Admin attempts to set `retain_ledgers = 0`. | Rejected with `Error::ConfigInvalid` (prevents instant unintentional deletion). |

### 5.2 Failure Modes

| Failure Mode | Root Cause | System Response |
|---|---|---|
| **Unconfirmed Execution** | Caller executes `apply_operational_cleanup` with `confirm == false`. | Rejected immediately with `Error::InvalidArgument`; zero storage mutations. |
| **Dry-Run Apply** | Caller executes `apply_operational_cleanup` with `plan.dry_run == true`. | Returns non-destructive no-op report (`destructive: false`). |
| **Unauthorized Policy Modification** | Non-admin caller calls contract policy mutators. | Gated at contract entry point; returns `Error::Unauthorized`. |
| **Missing Hold for Release** | Calling `release_operational_hold` for an ID without an active hold. | Returns `Error::NotFound`. |

---

## 6. Rollback and Recovery Steps

1. **Accidental Hold Placement**:
   - Call `release_operational_hold(env, &record_id)` to restore normal retention evaluation.
2. **Incorrect Retention Window Override**:
   - Call `set_operational_retention_policy` with corrected retention parameters, or clear the override key to immediately revert to safe compile-time defaults.
3. **Failed / Interrupted Cleanup Run**:
   - The cleanup engine is idempotent. Rerun `plan_operational_cleanup` to produce a fresh plan reflecting current on-chain state, then execute with confirmation.
4. **Data Recovery Post-Purge**:
   - On-chain deletion is permanent and final (preserving privacy and minimizing storage cost). Historical traceability for audit and compliance is maintained off-chain via emitted ledger events `("op_meta", "cleanup")` and indexed events.

---

## 7. Public API & Migration Compatibility

### 7.1 Existing Public APIs
Existing contract interfaces, event emissions, and public read paths remain untouched:
- No existing public struct or entry point is removed.
- Public storage keys live in their respective namespaces.

### 7.2 Migration Adapters
For upgrading legacy contracts storing raw key-value metadata:
```rust
let stored = minimize_legacy_metadata(
    &env,
    &record_id,
    OperationalModule::Aid,
    MetadataClass::Audit,
    &raw_keys,
    &raw_values,
    created_ledger,
)?;
```
To bridge into the workspace-level `shared::retention` system:
```rust
let retention_rec = to_shared_retention_record(&stored);
```

---

## 8. High-Cardinality Telemetry Protection

Telemetry metadata is the highest-cardinality and highest-volume operational data in the system. An adversary can flood the contract with unique label values (e.g. one unique `actor` or `seq` per call) to inflate storage, exhaust dashboard cardinality, and degrade observability for legitimate operators. This section defines the protection layer applied before telemetry is persisted or emitted.

### 8.1 Label Safety Rejection Rules

Before any telemetry label is accepted, it is validated by `reject_unsafe_label`. A label is rejected when any of the following hold:

| Rule | Constant | Rationale |
|---|---|---|
| Label key exceeds character budget | `MAX_LABEL_KEY_LEN = 64` | Prevents bloated keys that evade grouping. |
| Label value exceeds character budget | `MAX_LABEL_VALUE_LEN = 256` | Prevents embedded payloads in values. |
| Label count per sample exceeds budget | `MAX_LABELS_PER_SAMPLE = 16` | Bounds per-sample cardinality. |
| Label key is not in the allow-list | `ALLOWED_LABEL_KEYS` | Rejects unknown high-cardinality dimensions. |
| Label value contains control or non-printable bytes | `contains_control_bytes` | Prevents log injection and terminal escape abuse. |
| Label value matches a prohibited pattern | `PROHIBITED_LABEL_PATTERNS` | Rejects PII, credentials, and secrets at the edge. |

Rejections are counted in `TelemetryProtectionStats::rejected_labels` and surfaced through the `("op_meta", "telemetry_reject")` event so operators can alert on abuse without losing the rejected value.

### 8.2 Redaction Rules

Values that pass label validation are still subject to redaction. `redact_telemetry_value` applies the following in order:

1. **Known-sensitive keys are always redacted** (e.g. `secret`, `token`, `priv_key`, `email`, `phone`, `ip_addr`, `user_pii`, `biometric`, `card_no`, `api_token`, `dbg_trace`). The redacted form is `<redacted:<key>`.
2. **Account and address like values are hashed** to a stable, short fingerprint (`redact_hash`) so dashboards can group by actor without exposing the raw identifier.
3. **Free-text fields are truncated** to `MAX_REDACTED_LEN` and suffixed with `<trunc>`.
4. **Every redaction is counted** in `TelemetryProtectionStats::redacted_values`.

Redaction is deterministic and idempotent: applying it twice yields the same output, and already-redacted markers are not double-redacted.

### 8.3 Sampling Rules

Even with safe labels, telemetry volume can be abusive. `TelemetrySampler` applies a deterministic, ledger-seeded sampling policy:

| Policy | Constant | Behavior |
|---|---|---|
| Default sample rate | `DEFAULT_SAMPLE_RATE_PPM = 100_000` (10%) | One in ten samples persisted. |
| High-cardinality sample rate | `HIGH_CARDINALITY_SAMPLE_RATE_PPM = 10_000` (1) | Applied when a dimension exceeds `HIGH_CARDINALITY_THRESHOLD = 1_000` distinct values. |
| Emergency sample rate | `EMERGENCY_SAMPLE_RATE_PPM = 1`0 (0.01%) | Applied when `TelemetryProtectionStats::rejected_labels` exceeds `ABUSE_REJECT_THRESHOLD = 10_000` within a window. |
| Absolute ceiling | `MAX_TELEMETRY_RECORDS_PER_WINDOW = 5_000` | Hard cap on persisted telemetry records per retention window. |

Sampling is deterministic given `(seed, sample_id)`, so operators can reproduce a decision during incident review. Sampled-out records increment `TelemetryProtectionStats::sampled_out` and do not touch storage.

### 8.4 Dashboard Usefulness Under Abuse

When abuse is detected, the protection layer preserves useful signals:

- **Totals remain accurate** via `TelemetryProtectionStats::total_samples`, even when individual samples are dropped.
- **Rejection and redaction counts** are exposed as first-class metrics so operators can alert on abuse without storing abusive payloads.
- **Cardinality thresholds** are evaluated per dimension, so a single abusive dimension does not degrade unrelated dashboards.
- *(Sample rates are reported** in the `TelemetryProtectionStats` so dashboards can annotate charts with the effective sampling factor.

### 8.5 High-Cardinality Edge Cases & Failure Modes

| Scenario | Behavior | Handling |
|---|---|---|
| **One unique value per call** | Adversary emits a new label value every call. | High-cardinality threshold trips; sample rate drops to 1 per 1000 and the absolute ceiling bounds storage. |
| **Label value contains a secret** | Value matches a prohibited pattern. | Rejected at the edge; not stored, not emitted, counted in `rejected_labels`. |
| **Known-sensitive key** | Key is in the redaction list. | Value replaced with `<redacted:<key>`; counted in `redacted_values`. |
| **Account identifier in a free-text field** | Value looks account-like. | Hashed to a fingerprint; dashboards group by fingerprint instead of raw id. |
| **Sample seed mismatch** | Operator questions a sampling decision. | Sampling is deterministic given `(seed, sample_id)`; reproducible during incident review. |
| **Abuse window expires** | Rejections fall below the threshold. | Sample rate returns to the default 10% on the next window. |

### 8.6 Rollback and Recovery for Telemetry Protection

1. **False-positive rejections**:
   - Inspect the `(op_meta, telemetry_reject)` events to identify the rejected key and rule.
   - If the key is legitimate, add it to `ALLOWED_LABEL_KEYS` via the admin-policy mutator and re-deploy the configuration.
2. **False-negative redaction**:
   - Add the key to the redaction list and re-deploy. Existing records already stored are not rewritten; rotate them out via the normal retention window.
3. **Sampling too aggressive**:
   - Raise `DEFAULT_SAMPLE_RATE_PPM` or lower `HIGH_CARDINALITY_THRESHOLD` through the admin-policy mutator. Sampling changes take effect on the next window and do not rewrite history.
4. *(Storage ceiling reached**:
   - The absolute ceiling is a hard limit; excess samples are dropped and counted in `sampled_out`. Reduce the sample rate or wait for the retention window to roll forward.

---

## 9. Verification & Test Suite

The test suite in [`shared/src/operational_metadata.rs`](../shared/src/operational_metadata.rs) covers:
- Field classification across Treasury, Aid, Referral, Governance, Oracle, and Marketplace modules.
- Data minimization review stripping redundant and prohibited fields.
- Default retention policies and custom override validation.
- Metadata persistence, retrieval, and existence checks.
- Two-phase cleanup planning (dry-run non-destructiveness).
- Confirmed deletion of expired, eligible records.
- Active hold protection against deletion.
- Late hold placed between planning and execution.
- Invariant protection of frozen settlement and dispute classes against forged plans.
- Legacy metadata minimization adapter compatibility.
- High-cardinality label rejection for oversized, non-allowlisted, control-byte, and prohibited-pattern labels.
- Redaction of known-sensitive keys, account-like values, and free-text truncation.
- Deterministic sampling at default, high-cardinality, and emergency rates.
- Absolute ceiling enforcement and `sampled_out` accounting.
- Dashboard usefulness invariants: totals remain accurate while samples are dropped.
