# Trellis Operational Runbook

Incident triage and emergency rollback for the Trellis Soroban contracts.
This runbook is the single entry point on-call engineers follow when a
Trellis deployment misbehaves. It lists the incident categories, the triage
commands, the decision points, and the rollback / mitigation paths.

> **Never** paste secret keys, RPC tokens, or production credentials into an
> incident channel, PR, or shell history. Source configuration from the
> environment as described in [`CONFIGURATION.md`](./CONFIGURATION.md) and read
> secret redaction guidance in [`../SECURITY.md`](../SECURITY.md).

---

## 1. First five minutes

1. **Acknowledge** the alert and record the start time in the incident channel.
2. **Classify** using the table in §2.
3. **Freeze** non-incident deploys: pause merges to `main` and stop any
   in-flight `scripts/deploy.sh` runs.
4. **Capture state** before changing anything:

   ```bash
   # Repo health: formatting, lint, tests, build (same gates as CI).
   ./scripts/verify.sh

   # Contract/config diagnostics (RPC reachability, network id, flags).
   ./scripts/diagnostics.sh

   # Cross-contract wiring smoke test (read-only, see §7).
   ./scripts/verify-deployment.sh "$NETWORK"

   # Confirm the deployed network + contract ids recorded at deploy time.
   cat DEPLOYMENTS.md
   ```

5. **Open a timeline** and paste the outputs above (redact endpoints that embed
   credentials).

---

## 2. Incident categories

| Category | Typical signal | Primary risk | First action |
|---|---|---|---|
| Pause / liveness | Entry points revert with `ContractPaused` (5) | Funds frozen, no side effects | §3.1 |
| Authorization | `Unauthorized` (1) spikes, `role` errors | Unauthorized writes | §3.2 |
| Accounting / amounts | `InvalidAmount` (3), `Overflow` (4) | Mis-settlement, drift | §3.3 |
| Schema / migration | `UnsupportedSchemaVersion` (20), `SchemaMigrationFailed` (21) | Unreadable records | §3.4 |
| Upgrade | `UpgradeAlreadyPending` (906), `MigrationHookFailed` (905) | Stuck release | §3.5 |
| Escrow / payments | `PaymentEscrow*` (700–710) | Stuck or double release | §3.6 |
| Quota / abuse | `QuotaExceeded` (22) by many actors | Legit users blocked | §3.7 |
| Bulk imports | `BatchTooLarge` (940), `InvalidRow` (944), `DuplicateExternalId` (943) | Failed dataset ingest | §3.8 |
| Metadata retention | Unminimized leak, premature purge, hold collision | Lost audit / PII leak | §3.9 |

Error codes are defined in [`shared/src/errors.rs`](../shared/src/errors.rs) and [`shared/src/release_readiness.rs`](../shared/src/release_readiness.rs).

---

## 3. Triage procedures

### 3.1 Pause / liveness

**Decision:** is the contract intentionally paused, or paused by a bad release?

```bash
# Read-only: is the pause flag set? (getter name from the target contract)
soroban contract invoke \
  --id "$CONTRACT_ID" --network "$NETWORK" --source-account "$READ_ONLY_ACCOUNT" \
  -- is_paused
```

- **If paused unintentionally after a release** → treat as a bad release, go to
  §4 (Emergency rollback).
- **If pause was intentional** → confirm the owner (`get_admin`) and the
  expected unpause date, then communicate status.

### 3.2 Authorization

```bash
# Who is the admin?
soroban contract invoke --id "$CONTRACT_ID" --network "$NETWORK" \
  --source-account "$READ_ONLY_ACCOUNT" -- get_admin

# Does an account hold a role? (role enum per shared/src/auth.rs)
soroban contract invoke --id "$CONTRACT_ID" --network "$NETWORK" \
  --source-account "$READ_ONLY_ACCOUNT" -- has_role --user "$ADDRESS" --role Admin
```

**Decision:** if the admin is wrong or a role grant is missing, re-grant via the
admin path (`grant_role`), which requires the current admin signature. If the
admin key is suspected compromised, follow [`../SECURITY.md`](../SECURITY.md).

### 3.3 Accounting / amounts

```bash
# Reproduce the arithmetic path locally against the exact release.
git checkout "$RELEASE_TAG"
cargo test -p shared math:: -- --nocapture
cargo test -p payments-contract -- --nocapture
```

Check `shared/src/math.rs` helpers (`checked_add`, `checked_mul`, `apply_bps`)
before assuming a contract bug — most `Overflow` reports are pre-condition
violations. If a settlement is wrong, stop withdrawals (§3.6) and reconcile.

### 3.4 Schema / migration

```bash
cargo test -p shared compat:: -- --nocapture
```

- `UnsupportedSchemaVersion` on read → the reader is older than the written
  record. Redeploy the current build; do **not** hand-edit ledger entries.
- `SchemaMigrationFailed` on write → validate the record shape and re-run the
  migration path described in [`COMPATIBILITY.md`](./COMPATIBILITY.md).
- To backfill legacy records or reconcile missing derived indexes, run the
  operator-safe [Backfill Tool](BACKFILL_TOOL.md):
  `./scripts/backfill.sh --module <mod> --input <file> [--dry-run | --apply]`.

### 3.5 Upgrade

```bash
# What is registered / pending?
cargo test -p upgradeability -- --nocapture
./scripts/register_upgradeable.sh --help
```

**Decision:** if an upgrade is pending but stuck, complete or cancel it using
the registry flow in [`../UPGRADEABILITY.md`](../UPGRADEABILITY.md). Never
re-register a contract under a different name to work around a stuck upgrade.

### 3.6 Escrow / payments (stop the bleeding)

If value is at risk, prefer **pause + no new releases** over ad-hoc transfers:

```bash
# Pause state-changing paths (admin only).
soroban contract invoke --id "$CONTRACT_ID" --network "$NETWORK" \
  --source-account "$ADMIN_IDENTITY" -- set_paused --paused true
```

Then reconcile each escrow read-only before resuming:

```bash
soroban contract invoke --id "$CONTRACT_ID" --network "$NETWORK" \
  --source-account "$READ_ONLY_ACCOUNT" -- get_escrow --id "$ESCROW_ID"
```

### 3.7 Quota / abuse

```bash
# Inspect config + usage for a resource (see docs/QUOTA.md).
soroban contract invoke --id "$CONTRACT_ID" --network "$NETWORK" \
  --source-account "$READ_ONLY_ACCOUNT" -- get_quota_status \
  --actor "$ADDRESS" --resource aid_crt
```

**Decision:** reset a falsely-blocked actor (`reset_quota`) or raise limits with
`set_quota_config`. Treat a broad `QuotaExceeded` spike as an abuse signal and
escalate to security.

### 3.8 Operational metadata retention & minimization

For operational metadata data minimization reviews, retention enforcement, holds, and testable cleanup procedures across Treasury, Aid, Referral, Governance, Oracle, and Marketplace modules, follow [`docs/OPERATIONAL_METADATA_RETENTION.md`](./OPERATIONAL_METADATA_RETENTION.md).

- **Hold emergency**: Place an indefinite hold (`place_operational_hold`) to protect disputed operational metadata from automated cleanup runs.
- **Accidental override**: Clear or reset custom retention policy override to restore safe compile-time defaults.

---

## 4. Persistent storage TTL renewal

Persistent records must use the helpers exported by `shared::storage`:

- `persistent_set` writes the value and bumps it to `PERSISTENT_BUMP_AMOUNT`.
- `persistent_get` reads the value and refreshes the TTL when the key exists.
- `persistent_read` is reserved for an intermediate read that is guaranteed to
  be written in the same transaction; it must not be used for public query
  paths that need to keep records alive.
- `persistent_has` is an existence check only. Follow it with `persistent_get`
  when the record is needed.

Do not call `env.storage().persistent().set`, `.get`, or `.extend_ttl` directly
from a production contract. A new persistent key must be covered by a test
that advances the ledger close to `PERSISTENT_TTL_THRESHOLD`, reads or writes
the key through the helper, and verifies that the entry remains available.
The current seven-day bump and six-day refresh threshold are defined in
`shared/src/storage.rs`; change those constants deliberately and review the
gas impact before deployment.

### 3.8 Bulk import failures & remediation

When a bulk import (`import_aids`) encounters invalid rows or duplicate collisions:

1. **Check Dry-Run First**:
   Always execute `import_aids_dry_run` before committing. If errors are reported,
   refer to the returned `RollbackGuidance` and inspect `RowError` entries.
2. **Atomic Revert (`atom_rev`)**:
   Under `AllOrNothing` mode, any validation error prevents all persistent writes.
   Fix the identified rows in the source batch and re-submit.
3. **Partial Ingest (`part_rem`)**:
   Under `BestEffort` mode, valid rows are durably written while invalid rows are
   recorded in `report.errors`. Extract failing `row_id` entries, correct fields,
   and resubmit either as a delta batch or with `DuplicatePolicy::SkipExisting`
   to safely skip previously imported records.

See [`IMPORT_PIPELINE.md`](./IMPORT_PIPELINE.md) for complete validation rules and runbooks.

### 3.9 Feature rollout guard & rollback verification

When an alert reports an automatic rollback (`AutomaticRollbackTriggered` 963) or degraded rollout health:

1. **Check Rollout Guard Status**:
   ```bash
   soroban contract invoke \
     --id "$CONTRACT_ID" --network "$NETWORK" --source-account "$READ_ONLY_ACCOUNT" \
     -- get_guard_config --target "$TARGET_SYMBOL"
   ```
   Confirm that `stage` is `RolledBack` and `rollout_bps` is `0`.
2. **Review Rollback Audit**:
   Inspect the stored `RollbackAuditRecord` for `reason` (e.g. `ConsecutiveFailuresExceeded`, `ErrorRateBreached`, `InvariantViolation`) and `failing_error_code`.
3. **Execute Post-Rollback Verification**:
   ```bash
   soroban contract invoke \
     --id "$CONTRACT_ID" --network "$NETWORK" --source-account "$ADMIN_ACCOUNT" \
     -- verify_rollback --target "$TARGET_SYMBOL"
   ```
   Confirm `verified_clean: true`. If `verified_clean: false`, check `anomaly` (such as `SolvencyDiscrepancy` or `OrphanedPartialRecord`) and run module remediation before re-enabling.
4. **Re-arming Protection**:
   Do not attempt to re-enable the feature until `verify_rollback` has confirmed a clean state.

See [`FEATURE_FLAG_ROLLOUT_GUARD.md`](./FEATURE_FLAG_ROLLOUT_GUARD.md) for complete details.

### 3.10 Release readiness gate & rollback rehearsal failures

When a pull request, CI pipeline, or release promotion is blocked by the Release Readiness Gate:

1. **Diagnose Gate Blockers**:
   Execute the release readiness evaluation tool locally to isolate the failing gate:
   ```bash
   node scripts/release-readiness-gate.cjs --check-all
   ```
2. **Resolve by Gate Type**:
   - **Migrations Gate (`MissingMigrationVerification`, 971)**:
     Ensure schema versioning guards are active (`docs/STORAGE_SCHEMA_GUARDS.md`), run `shared::migration::dry_run(&plan)`, and ensure any breaking changes in `changelog/entries.json` contain non-empty `migration_notes`.
   - **Security Gate (`MissingSecurityCheck`, 972)**:
     Run `./scripts/security/run-audit.sh`. If an allowlist entry expired in `security/audit-allowlist.toml`, renew or remove it.
   - **Smoke Tests Gate (`MissingSmokeTest`, 973)**:
     Run `node scripts/verify-deployment.cjs <network>` to verify cross-contract getters and zero-value simulations across Treasury, Aid, Referral, Governance, Oracle, and Marketplace modules.
   - **Rollback Rehearsals Gate (`MissingRollbackRehearsal`, 974)**:
     Execute a simulated rollback rehearsal using `shared::release_readiness::rehearse_rollback` proving state preservation upon reverting to previous WASM.
3. **Emergency Bypass (Incident Only)**:
   If an emergency hotfix cannot await complete non-critical checks, append the documented exception marker to the PR body:
   ```markdown
   <!-- release-readiness-exception: <Incident ID, emergency rationale, and approving tech lead> -->
   ```
   See [`RELEASE_READINESS.md`](./RELEASE_READINESS.md) for full gate specifications.

## 5. Emergency rollback

Use when a release causes incorrect state changes or blocks critical
operations. Rollback is an **upgrade to the last known-good WASM**, plus the
documented migration/compat handling.

1. **Pause first** so no new side effects land during the rollback (§3.6).
2. **Identify the last known-good artifact** from `DEPLOYMENTS.md` and the
   release tag. *This is a decision point:* if state written by the bad release
   is not backward-compatible, stop and get the contract owner's sign-off before
   proceeding.
3. **Register and execute the rollback upgrade** following
   [`../UPGRADEABILITY.md`](../UPGRADEABILITY.md) and
   [`../DEPLOYMENTS.md`](../DEPLOYMENTS.md):

   ```bash
   # 1) re-register the known-good WASM hash
   ./scripts/register_upgradeable.sh --network "$NETWORK" --wasm "$GOOD_WASM"

   # 2) execute the upgrade (owner/admin signs)
   ./scripts/upgrade.sh --network "$NETWORK" --wasm-hash "$GOOD_WASM_HASH"

   # 3) record the new deployment for the next incident
   ./scripts/record-deployments.sh --network "$NETWORK" --note "rollback <incident>"
   ```

4. **Validate** with a read-only smoke test before unpausing:

   ```bash
   ./scripts/verify.sh
   ./scripts/diagnostics.sh
   ```

5. **Unpause** only after the smoke test passes, and post the outcome in §5.

> If an upgrade cannot be executed (e.g. no owner signature available), the
> mitigation is to **stay paused** and communicate. Do not move funds manually.

---

## 5. Communication template

```
INCIDENT <id> — <category> — <SEV>
Start (UTC):      <timestamp>
Impact:           <who/what>
Detected by:      <alert | report>
Current status:   <investigating | mitigated | resolved>
Last update:      <timestamp>
Next update:      <timestamp, <= 30 min>
Mitigation:       <pause | rollback | config change>
Owner:            @<on-call>
```

Update at least every 30 minutes until resolved, then publish a short
post-incident note: root cause, detection gap, and follow-up issues.

---

## 6. Reference

- [`FEATURE_FLAG_ROLLOUT_GUARD.md`](./FEATURE_FLAG_ROLLOUT_GUARD.md) — feature-flag rollout guard, automatic rollback triggers, and verification receipts.
- [`IMPORT_PIPELINE.md`](./IMPORT_PIPELINE.md) — bulk import validation, dry-run simulation, and remediation runbooks.
- [`DIAGNOSTICS.md`](./DIAGNOSTICS.md) — diagnostic tooling details.
- [`QUOTA.md`](./QUOTA.md) — quota limits and override procedure.
- [`COMPATIBILITY.md`](./COMPATIBILITY.md) — schema versions and migration.
- [`CONFIGURATION.md`](./CONFIGURATION.md) — env/secret requirements.
- [`../UPGRADEABILITY.md`](../UPGRADEABILITY.md) — upgrade registry and rollback.
- [`../DEPLOYMENTS.md`](../DEPLOYMENTS.md) — deployed contract ids per network.
- [`../SECURITY.md`](../SECURITY.md) — disclosure and key-compromise steps.

---

## 7. Deployment verification smoke test

After every testnet/mainnet deploy — and before declaring a deployment incident
resolved — run the cross-contract smoke test. It reads the recorded addresses
and queries each contract's read-only getters on-chain to prove the deployment
is wired together. `scripts/deploy.sh` can succeed while a missed
`scripts/initialize.sh` wiring step leaves claims and payouts failing on-chain;
this is the one command that catches that.

### 7.1 What it checks

| Check | Getter | Expected |
|---|---|---|
| Aid → Treasury | `AidContract::get_treasury` | deployed `treasury-contract` id |
| Treasury → Referral | `TreasuryContract::referral_contract` | deployed `referral-contract` id |
| Referral → Treasury | `ReferralContract::get_treasury` | deployed `treasury-contract` id |
| Registry coverage | `RegistryContract::get_contract(name)` | every deployed contract resolves to its recorded id |
| Invocation | read-only getter per core contract | contract reachable and callable by the configured source |

The invocation check is a zero-value, read-only simulation (`--send=no`); it
never signs or submits a state-changing transaction and never moves funds.

### 7.2 Usage

```bash
# Linux/macOS — run from the repo root
./scripts/verify-deployment.sh testnet

# Windows / cross-platform Node runner (same checks, same options)
node scripts/verify-deployment.cjs testnet
```

Common options (both runners):

| Flag | Purpose |
|---|---|
| `--network <net>` | `testnet` (default) or `mainnet` |
| `--deployments <file>` | Deployment JSON from `deploy.sh` (default `.deployment-log-<net>.json`) |
| `--deployments-md <file>` | Markdown fallback (default `DEPLOYMENTS.md`) |
| `--source <identity>` | Soroban source identity (default `admin`) |
| `--rpc-url <url>` | Override the RPC endpoint |
| `--registry-name k=sym` | Override the registry `Symbol` checked for contract `k` |
| `--skip-simulation` | Skip the invocation simulation |
| `--json` | Machine-readable summary (for CI) |

If the deployment JSON is absent the runner falls back to the network table in
`DEPLOYMENTS.md`. If neither has addresses it exits `2` with an actionable
error:

```
VERIFY ERROR: testnet contracts are not deployed yet (no addresses in ...)
  Fix: run ./scripts/deploy.sh testnet then ./scripts/record-deployments.sh testnet
```

### 7.3 Exit codes and output

| Exit | Meaning |
|---|---|
| `0` | every check passed |
| `1` | one or more checks failed (missing or mismatched address) |
| `2` | could not run (not deployed yet, `soroban`/`jq` missing) |

Every FAIL line names the getter, what it returned, what was expected, and the
fix — for example:

```
[FAIL] registry:referral
       RegistryContract::get_contract(referral) -> C... but Referral Contract is C...;
       update the registry entry
```

### 7.4 Registering missing registry entries

The default registry `Symbol` for each contract is its short name (`aid`,
`treasury`, `referral`, `registry`, `governance`, ...). If the deployment used
different names, pass `--registry-name <key>=<symbol>`. Register a missing
entry with:

```bash
soroban contract invoke \
  --id "$REGISTRY_ID" --network "$NETWORK" --source admin \
  -- set_contract --caller admin --name referral \
     --address "$REFERRAL_ID" --version 1
```

### 7.5 CI / automation

```bash
./scripts/verify-deployment.sh testnet --json
```

prints `{"network":...,"pass":N,"fail":N,"checks":[...]}` and exits non-zero on
any failure, so it can gate a post-deploy job. It is intentionally **not** part
of the PR CI job set, because it requires a live network and a funded source
identity.
