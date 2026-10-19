# Deterministic Recovery Flow (Fixes #49)

The implementation lives in [`shared::recovery`](shared/src/recovery.rs). This document covers the deterministic duplicate detection and merge review workflow added in this issue, alongside the existing checkpoint state machine.

## Problem

Candidate duplicates arrive from many sources (replayed webhooks, retries, concurrent workers). We need to detect them reproducibly, review merges before they are applied, and reverse them when possible, all with a full audit trail.

## Deterministic duplicate detection

Duplicate detection is driven by a deterministic canonical key derived from the candidate record. The key is computed with the same algorithm on every node, so candidates that are equivalent under the normalization rules always produce the same key.

- The key is derived from normalized fields (lowercased identifiers, trimmed strings, canonical numeric encoding).
- No wall-clock, randomness, or iteration order is used in key derivation.
- The same input always yields the same key and the same duplicate grouping.

The detector returns groups of candidates sharing a key. Groups are ordered by the key and members within a group are ordered by candidate id, so the output is stable across runs and nodes.

## Merge review workflow

A merge is a reviewable record, not an immediate effect. It moves through explicit states:

```
  Proposed  -->  Approved  -->  Applied
     |              |
     v              v
  Rejected        Reverted
(from Applied)
```

| State | Meaning |
|---|---|
| Proposed | created from a duplicate group; awaiting review |
| Approved | reviewer accepted; ready to apply |
| Applied | effect applied; reversible while not finalized |
| Rejected | terminal; no effect was applied |
| Reverted | terminal; applied effect was undone |

Every transition is recorded in the audit log with the actor, timestamp, from-state, to-state, and a reason code. Transitions are guarded: an illegal transition returns an error and leaves the record unchanged.

## Reversability

Applying a merge is reversible until it is finalized. Reversion is allowed only from `Applied` and only while the record is not finalized. Reversion restores the pre-merge state and appends a `Reverted` audit entry. Once finalized, the merge is terminal and cannot be reverted through this workflow.

## Audit

The audit log is append-only. Each entry records the merge id, the action, the actor, the timestamp, and the resulting state. Audit entries are never mutated or deleted; corrections are expressed as new entries.

## Tests

```bash
cargo test -p shared recovery
```

The suite covers:

- Deterministic key derivation and stable group ordering across repeated runs.
- Detection of duplicates from normalized-equivalent inputs.
- Merge state transitions, including illegal transition rejection.
- Reversion from `Applied` and rejection of reversion once finalized.
- Audit entry appending for every transition.

## Failure modes and recovery

- Detection returns inconsistent groups - this indicates a non-deterministic input or corrupted candidate data. Re-run detection on the same snapshot; if it remains inconsistent, quarantine the candidates and escalate.
- Merge approved but not applied - safe to retry apply; the apply step is idempotent and guarded by the state machine.
- Merge applied but review is disputed - revert from `Applied` while not finalized; the audit log preserves the full history.
- Attempted reversion after finalization - rejected by the state machine; open a new merge proposal instead.
- Audit gap detected - treat the merge as untrusted; do not apply or reverse until the audit log is reconciled.

## Rollback and recovery steps

1. Identify the merge id and inspect its audit log.
2. If the merge is `Applied` and not finalized, issue a reversion and confirm the `Reverted` audit entry.
3. If the merge is `Proposed` or `Approved`, reject it; no effect was applied.
4. If the merge is finalized, do not attempt reversion; reconcile out-of-band and open a new proposal.
5. Re-run detection on the current snapshot to confirm no duplicate groups remain unreviewed.

## Restore Validation

For checking invariants after a restore or migration, see [Restore Validation](RESTORE_VALIDATION.md).
