# Operator-Safe Backfill Tool Framework (Issue #217)

The Trellis Backfill Tool provides maintainers and on-call operators with a safe, deterministic, and resumable mechanism for backfilling historical records, reconciling derived indexes, and applying schema migrations across Soroban smart contracts.

The framework supports the core modules of Trellis:
- **Treasury Contract** (`contracts/treasury-contract`)
- **Aid Contract** (`contracts/aid-contract`)
- **Referral Contract** (`contracts/referral-contract`)
- **Governance Contract** (`contracts/governance-contract`)
- **Oracle Contract** (`contracts/oracle-contract`)
- **NFT Marketplace** (`contracts/nft-marketplace`)

The tool implementation lives in:
- CLI Executable: [`scripts/backfill.cjs`](../scripts/backfill.cjs) (and shell wrapper [`scripts/backfill.sh`](../scripts/backfill.sh))
- Core Engine & Types: [`shared::backfill`](../shared/src/backfill.rs)
- Automated Test Suite: [`scripts/test-backfill.cjs`](../scripts/test-backfill.cjs)

---

## 1. Core Principles

1. **Dry-Run by Default**: Unless `--apply`, `--resume`, or `--rollback` is explicitly passed, the tool executes purely in memory or against read-only ledger simulations. It never mutates state or writes checkpoint files during dry-run.
2. **Deterministic Checkpoints**: Checkpoints are persisted atomically after every batch. If execution halts due to an invalid record, system crash, or network timeout, operators can resume without duplicate mutations or lost progress.
3. **Transparent Field-Level Diff**: Every record modification displays a before/after field diff (`+` addition, `-` deletion, `~` change) prior to execution.
4. **Strict Skip vs Failure Categorization**: Records that are already compliant and identical are skipped (no-ops). Invalid records are recorded with exact error codes, causes, and actionable remediations.
5. **Reversible Mutations**: An automated inverse mutation journal is recorded in each checkpoint, enabling rapid 1-command rollback.

---

## 2. CLI Command Reference

### Basic Invocations

```bash
# Preview changes with dry-run diff (read-only, default)
./scripts/backfill.sh --module aid --input fixtures/backfill/aid.json

# Apply backfill changes in batches of 25
./scripts/backfill.sh --module aid --input fixtures/backfill/aid.json --apply

# Inspect active checkpoint status
./scripts/backfill.sh --module aid --status

# Resume an interrupted backfill from the last checkpoint
./scripts/backfill.sh --module aid --input fixtures/backfill/aid.json --resume

# Roll back all changes committed in the last run
./scripts/backfill.sh --module aid --rollback

# Run all modules sequentially in dependency order
./scripts/backfill.sh --module all --input fixtures/backfill/
```

### Options & Flags

| Flag | Description | Default |
|---|---|---|
| `--module <name>` | Target module (`aid`, `treasury`, `referral`, `governance`, `oracle`, `marketplace`, `all`) | Required |
| `--input <file>` | Path to JSON backfill records | Required (except status/rollback) |
| `--dry-run` | Simulate and preview field diffs without writing state | `true` |
| `--apply` | Execute mutations and persist checkpoints | `false` |
| `--resume` | Resume execution from the last checkpoint cursor | `false` |
| `--rollback` | Revert mutations using the rollback journal | `false` |
| `--status` | Display progress and metrics from active checkpoint | `false` |
| `--clear-checkpoint` | Clear checkpoint after successful completion | `false` |
| `--batch-size <n>` | Records per batch checkpoint commit (1..100) | `25` |
| `--mode <mode>` | `all-or-nothing` (abort on error) or `best-effort` (skip failing records) | `best-effort` |
| `--duplicate-policy <pol>` | Duplicate handling: `skip`, `update`, or `reject` | `skip` |
| `--target-state <file>` | Path to target state snapshot (for offline validation) | `.state-<module>.json` |
| `--checkpoint <file>` | Custom checkpoint file path | `.backfill-checkpoint-<module>.json` |
| `--json` | Emit machine-readable JSON output | `false` |

---

## 3. Module Schemas & Validation Rules

### 3.1 Aid Module (`aid`)
- **Key**: `id` (numeric string/integer)
- **Required fields**: `donor` (valid Address), `recipient` (valid Address), `amount` (> 0), `expiry` (> 0), `status` (`Pending`, `Settled`, `Refunded`), `schema_version` (must be `2`).
- **Invariants**: Expiration must be future; amount cannot be zero or negative.

### 3.2 Treasury Module (`treasury`)
- **Key**: `<token>:<category>`
- **Required fields**: `token` (Address), `category` (Symbol, e.g. `reserve`, `rewards`, `yield`), `balance` (non-negative integer).
- **Invariants**: Balances must be >= 0; total allocations must not exceed token reserves.

### 3.3 Referral Module (`referral`)
- **Key**: `referee` (Address)
- **Required fields**: `referee` (Address), `referrer` (Address), `lifetime_accrued` (>= 0).
- **Invariants**: Self-referrals (`referee == referrer`) are strictly rejected. Circular graphs are forbidden.

### 3.4 Governance Module (`governance`)
- **Key**: `param:<key>` or `admin_set`
- **Required fields**:
  - Parameter: `key` (String), `value` (non-null).
  - Admin set: `admins` (Array of Addresses, max 20), `threshold` (`1 <= threshold <= admins.len()`), `quorum_bps` (`0..10000`).
- **Invariants**: Threshold cannot exceed total admin count; quorum BPS cannot exceed 100%.

### 3.5 Oracle Module (`oracle`)
- **Key**: `asset` (e.g. `XLM/USD`, `USDC/USD`)
- **Required fields**: `asset` (String), `price` (> 0), `timestamp` (> 0).
- **Invariants**: Price must be positive; timestamp cannot be future-skewed beyond 60s.

### 3.6 Marketplace Module (`marketplace`)
- **Key**: `col:<collection_id>` or `list:<listing_id>`
- **Required fields**:
  - Collection: `collection_id`, `name`, `symbol`, `owner`, `royalty_bps` (`0..10000`), `metadata_hash` (64 hex characters).
  - Listing: `listing_id`, `price` (> 0).
- **Invariants**: Royalty BPS cannot exceed 10,000; metadata hash must be exactly 32 bytes (64 hex chars).

---

## 4. Checkpoint Architecture & Resume Mechanics

### Checkpoint State Machine

```
              ┌───────────────┐
              │  NotStarted   │
              └───────┬───────┘
                      │ begin_backfill
                      ▼
              ┌───────────────┐  advance_checkpoint (all items processed)
              │  InProgress   │ ──────────────────────────────────────────► ┌───────────┐
              └───────┬───────┘                                             │ Completed │
       failure        │   resume                                            └───────────┘
   (all-or-nothing)   ▼     ▲
              ┌──────────────────┐               rollback
              │ AwaitingRecovery │ ───────────────────────────────────────► ┌────────────┐
              └──────────────────┘                                          │ RolledBack │
                                                                            └────────────┘
```

### Checkpoint File Schema (`.backfill-checkpoint-<module>.json`)

```json
{
  "runId": "run_1729050000000",
  "module": "aid",
  "status": "InProgress",
  "mode": "best-effort",
  "cursor": 50,
  "totalRecords": 100,
  "processedKeys": ["1", "2", "3", ...],
  "stats": {
    "scanned": 50,
    "created": 40,
    "updated": 5,
    "skipped": 4,
    "failed": 1
  },
  "skippedRecords": [
    { "index": 12, "key": "12", "reason": "AlreadyUpToDate" }
  ],
  "failedRecords": [
    { "index": 34, "key": "34", "error": "ValidationError", "reason": "Amount must be positive, got 0" }
  ],
  "rollbackJournal": [
    { "action": "CREATED", "key": "1" },
    { "action": "UPDATED", "key": "2", "previousValue": { ... } }
  ],
  "startedAt": 1729050000000,
  "updatedAt": 1729050015000,
  "lastError": null
}
```

### How Resume Operates
When `--resume` is invoked:
1. The tool reads the checkpoint and verifies `status != Completed`.
2. It loads `cursor` and the `processedKeys` set.
3. All records before `cursor` or with keys in `processedKeys` are skipped immediately without executing duplicate network calls or mutations.
4. Execution resumes from `cursor`, updating the checkpoint every batch until all items complete.

---

## 5. Operational Edge Cases & Failure Modes

| Edge Case | Root Cause | Tool Behavior | Remediation |
|---|---|---|---|
| **Process Crash / SIGKILL** | Power failure, killed container, out-of-memory. | Checkpoint was atomically committed after the last completed batch. Prior batch mutations are recorded in the state and checkpoint. | Re-run command with `--resume`. The tool automatically skips completed records and continues. |
| **RPC / Network Timeout** | Transient node overload or network drop. | Batch transaction fails; under `all-or-nothing`, checkpoint transitions to `AwaitingRecovery`. | Wait for RPC availability; run with `--resume`. |
| **Duplicate Records in Input** | Source dataset contains multiple rows for the same key. | Handled via `--duplicate-policy`: `skip` skips subsequent duplicates; `update` updates state with latest; `reject` reports `DuplicateRecordInBatch`. | Clean source batch or set `--duplicate-policy skip`. |
| **Invariant Violation** | Proposed balance is negative, referral cycle, or invalid quorum. | Record is flagged with `InvariantViolation` error; under `all-or-nothing`, execution pauses. | Correct offending row in dataset and run with `--resume`. |
| **Concurrent Writes Drift** | Another admin/worker modified the record while backfill was queued. | Dry-run diff highlights discrepancy (`~ field: current -> proposed`). | Review diff in `--dry-run` to ensure proposed update is still desirable. |

---

## 6. Rollback Runbook

If a backfill introduces erroneous data or needs to be undone:

1. **Stop all in-flight backfill processes**:
   ```bash
   pkill -f "scripts/backfill" || true
   ```
2. **Inspect the checkpoint status**:
   ```bash
   ./scripts/backfill.sh --module <mod> --status
   ```
3. **Execute rollback**:
   ```bash
   ./scripts/backfill.sh --module <mod> --rollback
   ```
4. **Verify restoration**:
   - Newly created records (`action: CREATED`) are deleted.
   - Updated records (`action: UPDATED`) are reverted to their `previousValue`.
   - Run restore validation checks:
     ```bash
     ./scripts/validate-restore.sh
     ```
5. **Clear the checkpoint after verification**:
   ```bash
   ./scripts/backfill.sh --module <mod> --clear-checkpoint
   ```

---

## 7. Automated Testing & Verification

Run the automated test suite to verify backfill tool behavior across all modes and modules:

```bash
node scripts/test-backfill.cjs
```
