# Release Readiness Runbook (Issue #299)

## Overview
This runbook establishes protocol release verification gates across Trellis contracts: Treasury, Aid, Referral, Governance, Oracle, and Marketplace. Before any on-chain deployment, contract upgrade, or release tag promotion, releases must satisfy all four operational gates.

---

## The Four Release Gates

```
+-----------------------------------------------------------------------------------+
|                            Release Manifest Gates                                 |
+-----------------------------------------------------------------------------------+
|  1. Migration Gate       | Schema version order, dry-run journal, 0 unmigrated     |
|  2. Security Gate        | Fresh audit allowlist, invariant assertions, admin roles|
|  3. Smoke Tests Gate     | RPC probe, getter liveness, client bindings intact      |
|  4. Rollback Rehearsal   | Pre-upgrade snapshot, Revert rehearsal passed           |
+-----------------------------------------------------------------------------------+
```

### 1. Migration Gate
- **Objective**: Ensures storage schemas (V1, V2, etc.) are forward-compatible and data migrations leave no orphaned or unmigrated entries.
- **Requirements**:
  - `target_version > current_version`.
  - `has_dry_run_journal == true`.
  - `unmigrated_records == 0`.
  - `changelog/entries.json` has complete migration notes for breaking changes.

### 2. Security Gate
- **Objective**: Validates multi-tenant boundaries, access control roles, and threat invariants.
- **Requirements**:
  - All admin keys verified and two-step transfers completed.
  - Zero unverified emergency pauses.
  - Audit allowlists refreshed within 7 days.

### 3. Smoke Tests Gate
- **Objective**: Proves that deployed bytecode is active and callable on testnet or staging.
- **Requirements**:
  - RPC probe `/health` returns healthy status.
  - Public getters (`get_public_status`, `get_risk_policy`, etc.) return valid records without panics.
  - Client bindings pass type checks without method signature drift.

### 4. Rollback Rehearsal Gate
- **Objective**: Guarantees that in the event of an upgrade failure, state and funds can be safely reverted without loss.
- **Requirements**:
  - Pre-upgrade state snapshot persisted in storage.
  - Rollback strategy must be `Revert` or `ForwardFix` (`Manual` is disallowed for automated gates).
  - Rehearsal simulation completes with `state_intact == true` and receipt issued.

---

## Authorization & Signoff Procedures

### Standard Release Signoff
1. Evaluate manifest using `evaluate_release_readiness(&env, &manifest)`.
2. Ensure report status `is_ready == true`.
3. Authorized administrator signs off on release using `signoff_release(&env, &admin, &release_version)`.

### Emergency Bypass Signoff
In the event of an urgent hotfix where one non-critical gate must be bypassed:
1. Two distinct protocol administrators must execute `emergency_bypass_signoff`.
2. Emits an immutable `emrg_byp` audit event with reason and timestamp.
