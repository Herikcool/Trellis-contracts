# Tamper-Evident Audit Ledger

`shared::audit_ledger` (Issue #233) is an append-only, hash-chained ledger for
the **privileged** and **financial** events of the Trellis contracts: role and
admin changes, configuration changes, upgrades, pause/unpause, and every
movement or commitment of value across Treasury, Aid, Referral, Governance,
Oracle and Marketplace.

Soroban events are emitted for indexers, but they are not individually
verifiable: a consumer cannot prove that none was inserted, dropped, reordered,
or edited before it arrived, and the contract keeps no record it can re-derive.
The audit ledger gives every privileged/financial event a position in an ordered
chain whose integrity the contract itself can re-check at any time.

## Design in one paragraph

Each entry is written exactly once at `seq = length + 1`. Its hash covers every
field, plus the previous entry's hash and a fixed domain separator, so editing
any entry — including an old one — changes that entry's hash, every later
`prev_hash` link, and the stored head. The head (`length`, `head_hash`,
`last_ledger`, `last_timestamp`) is the verifiable checkpoint. Verification
re-derives the whole chain from storage and reports the **first** inconsistency
instead of a bare boolean, so an operator can bound how much prefix to trust.

```text
prev_hash(1) = 0x00..00
hash(n)      = sha256( xdr([
                 "trellis_audit_ledger", schema_version,
                 seq, class, kind, module, actor, subject, resource, attribute,
                 amount, previous_amount, note, correlation_id,
                 ledger, timestamp, prev_hash
               ]) )
```

The encoding is Soroban's canonical `Val` XDR (`ToXdr`), not a bespoke string
format, so an off-chain verifier reproduces the hash exactly. `note` is a short
`Symbol` reason/action code: free-form text is deliberately rejected so the
ledger cannot become an accidental PII sink. `correlation_id` uses the all-zero
hash for "absent" because `Option<BytesN<32>>` is not encodable in a Soroban
contract type.

## Public surface

| Item | Purpose |
|---|---|
| `append_event(env, params)` | Append one event; returns the stored entry. |
| `append_privileged` / `append_financial` | Typed wrappers that reject a kind from the other class. |
| `head(env)` / `checkpoint(env)` / `length(env)` | Current length + head hash anchor. |
| `get_entry(env, seq)` | One entry by sequence. |
| `entry_hash(env, entry)` | Recompute an entry hash off-chain or on-chain. |
| `verify_chain(env)` | Full re-derivation and comparison with the head. |
| `export_ledger(env, reader, from_seq, limit)` | Bounded, permissioned export bundle. |
| `verify_export(env, export)` | Storage-free verification of an export bundle. |
| `export_digest(env, export)` | Recompute a bundle digest after re-serialization. |
| `publish_head_checkpoint(env)` | Emit `("audit","head")` with the current anchor. |

`AuditEventKind` is the closed taxonomy. Variants are stable: add new variants
with new numeric codes, never reuse or renumber one, because the code is part of
the entry hash. `AuditEventKind::class` maps each kind to `Privileged` or
`Financial`.

### Adopting the ledger in a contract

The ledger is opt-in and additive. Nothing in `shared::events`,
`shared::timeline`, or any contract interface changes.

```rust
use shared::audit_ledger::{append_event, AuditEventKind, AuditEventParams};

// Inside a privileged/financial entry point, after authorization:
append_event(&env, AuditEventParams {
    kind: AuditEventKind::TreasuryWithdrawal,
    module: symbol_short!("treasury"),
    actor: caller.clone(),
    subject: Some(recipient),
    resource: Some(token),
    attribute: Some(category),
    amount: Some(amount),
    previous_amount: Some(balance_before),
    note: Some(symbol_short!("funds_out")),
    correlation_id: genesis_hash(&env), // or the caller's id
})?;
```

Call it **after** the contract has authorized the caller and decided the action
will proceed. The ledger records; it does not authorize. Because Soroban rolls
back the whole invocation on error, an append that fails leaves no partial entry.

## Verification

Run the focused suite:

```bash
cargo test -p shared test_audit_ledger
```

On-chain, any caller can run `verify_chain`; the contract re-reads every entry,
recomputes each hash, and compares the re-derived head with the stored head.

Operationally, anchor the chain on a schedule:

1. Call `publish_head_checkpoint` (or read `head`) and store `(length,
   head_hash)` off-chain. This is a commitment: a chain that later reproduces
   that hash is the same chain.
2. Export windows with `export_ledger` (admin only — `Permission::ReadAuditTrail`)
   and archive them. Each bundle is self-contained: `start_prev_hash` anchors
   the first entry, each entry carries its hash, and `digest` covers the
   metadata and every entry.
3. Verify archives with `verify_export` and the published checkpoint. A bundle
   that reaches the head must end on the checkpoint hash.

### Limits and cost

- Verification is bounded to `MAX_VERIFY_ENTRIES` (10,000) entries per call;
  larger ledgers return `ScanLimitExceeded`. Verify large ledgers by exporting
  and verifying ranges against a checkpoint.
- Exports return at most `MAX_EXPORT_LIMIT` (100) entries per bundle, defaulting
  to `DEFAULT_EXPORT_LIMIT` (20) when the caller passes `0`.
- Each append costs one persistent write plus one instance write (head) plus
  hashing. Entries use the workspace persistent TTL policy; the head is instance
  storage.
- Multiple events may share a ledger sequence. Ordering within a ledger is by
  append order (`seq`), with timestamps required to be non-decreasing.

## Operational edge cases

| Situation | Behavior |
|---|---|
| Two events in the same ledger | Allowed; `seq` orders them, timestamp ties are allowed, a lower timestamp is rejected. |
| Ledger sequence goes backwards | `append_event` returns `InvalidArgument` and writes nothing. |
| Appending after an upgrade/rollback restored an older snapshot | The head reflects the snapshot; appends resume from it. If entries exist beyond the head, `verify_chain` reports `UnexpectedTail` instead of silently accepting them. |
| Restoring a contract from a ledger snapshot | The ledger is restored with the snapshot. Any divergence from the published checkpoint is detectable; a snapshot older than the checkpoint is a tamper/rollback event, not a normal restore. |
| Persistent entry eviction | Entries are bumped to the 7-day TTL window on append and read. A gap caused by manual TTL eviction surfaces as `MissingEntry`, never as a silent successful verification. |
| Schema evolution | Bump `AUDIT_LEDGER_SCHEMA_VERSION` and add a migration note. Old entries keep their recorded version and their hash stays verifiable; do not rehash historical entries. |
| Non-admin export attempt | Rejected with `Unauthorized` before any entry is read. |
| Oversized export request | Capped at `MAX_EXPORT_LIMIT`; `has_more` tells the caller to continue from `to_seq + 1`. |
| `from_seq == length + 1` | Returns an empty bundle anchored at the head, so a catch-up client can prove it is current. |
| Actor/subject PII | Only addresses and short `Symbol` codes are recorded; `note` is not free text. Pair with `shared::operational_metadata` retention if audit records become personal data. |

## Failure modes

`verify_chain` and `verify_export` report `failure` and `first_invalid_seq`.
`first_invalid_seq == 0` means verified.

| Failure | Meaning | Blast radius | Immediate action |
|---|---|---|---|
| `MissingEntry` | A sequence in `1..=length` has no stored entry. | Everything after the gap is unverifiable. | Halt privileged/financial writes; restore the missing prefix from an archive. |
| `NonMonotonicSequence` | A stored `seq` does not match its storage key (swap/rekey). | The entry and all later links. | Treat as tampering; restore from the last verified export. |
| `HashMismatch` | An entry's contents no longer hash to its `hash` field. | That entry; later entries remain linked to the tampered hash only if rehashed. | Freeze writes; export `1..first_invalid_seq - 1` as the trusted prefix. |
| `BrokenLink` | An entry's `prev_hash` does not match the previous entry's hash (edit + rehash, or insertion). | The break point and everything after. | Treat as insertion/rewrite; restore from checkpoint-backed archive. |
| `HeadMismatch` | The stored head is not the hash the chain actually produces. | The head anchor. | Re-anchor only after root-causing: publish the re-derived hash and a new checkpoint. |
| `UnexpectedTail` | Entries exist beyond the committed head (head rolled back, or entry inserted without head update). | The tail entries. | Decide whether an upgrade rollback or an attack caused it; restore/roll forward, then re-verify. |
| `LedgerRegression` | Entry ledger sequence moves backwards. | Ordering guarantee. | Restore ordering from archive; investigate how a back-dated write was possible. |
| `TimestampRegression` | Timestamp moves backwards within one ledger. | Ordering guarantee for same-ledger events. | Same as above. |
| `ExportDigestMismatch` | Bundle metadata or entries were modified after packaging. | The bundle. | Discard the bundle and re-export from chain. |
| `ExportMetadataMismatch` | Range/head metadata is internally inconsistent (or unsupported schema version). | The bundle. | Discard; check the exporter and schema version. |
| `ScanLimitExceeded` | Ledger exceeds `MAX_VERIFY_ENTRIES`. | None; bounded-work guard. | Export and verify in ranges against a checkpoint. |

## Rollback and recovery

The ledger itself is never rolled back in place: there is no update, delete, or
truncate API. Recovery always means **bounding the trusted prefix and
re-establishing a head**.

1. **Halt.** Pause the affected contract/module (`shared::storage::set_paused`
   or the contract's pause entry point) so no new privileged/financial events
   are appended while the state is in question.
2. **Bound the prefix.** Run `verify_chain`. `first_invalid_seq - 1` entries are
   provably intact; export `1..first_invalid_seq - 1` and store the bundle with
   its digest.
3. **Preserve evidence.** Archive the failing entry and the raw storage
   snapshot before any restore. Do not rehash or delete the tampered entry.
4. **Root-cause.** `UnexpectedTail`/`HeadMismatch` after an upgrade usually means
   an upgrade rollback restored an older instance; treat it as a rollback event
   and re-verify against the checkpoint published before the upgrade. A
   `HashMismatch` or `BrokenLink` with no upgrade means storage was modified
   outside the contract and should be treated as a security incident.
5. **Restore or roll forward.**
   - *Restore path*: redeploy the last known-good WASM, restore from a ledger
     snapshot at or before the last verified checkpoint, then re-run
     `verify_chain` and compare the head with the published checkpoint.
   - *Roll-forward path* (preferred when the prefix is intact and only the head
     is wrong): leave history untouched, publish the re-derived head as a new
     checkpoint, and continue appending. Record the incident in
     `docs/RECOVERY.md` and the changelog if any schema changed.
6. **Re-anchor and resume.** Call `publish_head_checkpoint`, publish the new
   `(length, head_hash)` to indexers, and only then unpause.
7. **Re-verify.** After resuming, `verify_chain` must be clean and `verify_export`
   of the `1..length` range must reproduce the new checkpoint.

### Recovery participants

- **Maintainers** own step 1 (halt), step 5 (restore/roll forward), and step 6
  (unpause). The pause primitive is `shared::storage::set_paused`; the upgrade
  path is `UPGRADEABILITY.md`.
- **Indexers** must adopt the checkpoint as a reorg boundary: drop unverified
  tail events and re-index from the trusted prefix.
- **Auditors** independently re-run `verify_export` on archived bundles against
  the published checkpoint; they need no contract access.

## Testing

`shared/src/test_audit_ledger.rs` is the executable form of this document: 26
tests cover chain construction, chronology rejection, every tamper mode
(edit/hash/rehash/link/delete/swap/head-rollback/head-edit), export permission,
pagination, digest and head attacks, empty-ledger and empty-tail bundles,
storage reload, and the checkpoint event.

```bash
cargo test -p shared test_audit_ledger
```
