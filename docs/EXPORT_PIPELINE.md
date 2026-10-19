# Permission-Aware Export Pipeline & Streaming Runbook

The Trellis Permission-Aware Export Pipeline provides a secure, gas-bounded, and stream-safe mechanism for exporting operational, financial, and governance datasets across Soroban smart contracts.

The pipeline spans the six core modules of Trellis:
- **Treasury Contract** (`contracts/treasury-contract`)
- **Aid Contract** (`contracts/aid-contract`)
- **Referral Contract** (`contracts/referral-contract`)
- **Governance Contract** (`contracts/governance-contract`)
- **Oracle Contract** (`contracts/oracle-contract`)
- **NFT Marketplace** (`contracts/nft-marketplace`)

The implementation lives in:
- Core Engine & Types: [`shared::export`](../shared/src/export.rs)
- Verification Runner: [`scripts/verify-export-pipeline.cjs`](../scripts/verify-export-pipeline.cjs)

---

## 1. Architecture & Design Principles

| Feature | Mechanism | Operational Benefit |
|---|---|---|
| **Bounded Memory Streaming** | Incremental chunking bounded by `DEFAULT_EXPORT_CHUNK_SIZE` (25) and `MAX_EXPORT_CHUNK_SIZE` (50) | Guarantees Soroban transactions never exhaust memory or compute gas budgets during large exports. |
| **In-Flight Cancellation** | `cancel_export_stream` transitions session to `Cancelled` | Operators or initiators can halt rogue or runaway streams; all subsequent chunk writes are rejected. |
| **Multi-Module Authorization** | `authorize_module_export` enforces module roles across Treasury, Aid, Referral, Governance, Oracle, Marketplace | Prevents privilege escalation and restricts sensitive operational data to authorized personnel. |
| **Defense-in-Depth Redaction** | Forbidden field purging (`purge_forbidden_fields`) + safe-field allow-list (`redact_fields`) | Secret keys, seeds, entropy, PII, and raw credentials can never leak into an export artifact. |
| **Chained Integrity Digests** | Cumulative SHA-256 rolling digest across all streamed chunks | Consumers can verify that stream chunks have not been reordered, omitted, or tampered with. |
| **Backward Compatibility** | Existing `generate_export` and `create_export` APIs remain unchanged | Full compatibility with V1 export consumers while adding additive streaming capabilities. |

---

## 2. Multi-Module Permission Matrix

Access to export data is governed by the caller's role, requested module, and export scope:

| Module | `ExportScope::OwnRecords` | `ExportScope::Operational` | `ExportScope::Full` |
|---|---|---|---|
| **Treasury** | Authenticated caller (own account movements) | `Role::TreasuryManager` or `Role::Admin` | `Role::Admin` only |
| **Aid** | Authenticated caller (own claims & allocations) | `Role::Support` or `Role::Admin` | `Role::Admin` only |
| **Referral** | Authenticated caller (own referral reward graph) | `Role::ReferralManager` or `Role::Admin` | `Role::Admin` only |
| **Governance** | Authenticated caller (own proposals & votes) | `Role::Support` or `Role::Admin` | `Role::Admin` only |
| **Oracle** | Authenticated caller (`Role::OracleSigner`) | `Role::OracleSigner` or `Role::Admin` | `Role::Admin` only |
| **Marketplace** | Authenticated caller (own listings, bids, orders) | `Role::Support` or `Role::Admin` | `Role::Admin` only |

*Note: Any caller lacking the necessary role is immediately rejected with `ExportError::ScopeDenied` (`920`).*

---

## 3. Streaming Pipeline State Machine & Workflow

```
       [ Init Request ]
              │
              ▼
       ┌──────────────┐
       │    Active    │◄──────────────┐
       └──────┬───────┘               │
              │                       │
     stream_export_chunk              │
              │                       │
              ├── [ Total < Max ] ────┘
              │
              ├── [ Total >= Max or finalize ] ──► ┌───────────────┐
              │                                    │   Completed   │
              │                                    └───────────────┘
              └── cancel_export_stream ──────────► ┌───────────────┐
                                                   │   Cancelled   │ (Terminal)
                                                   └───────────────┘
```

### Pipeline Lifecycle Methods

1. **`init_export_stream(env, caller, module, request, session_id) -> ExportSession`**
   - Validates export parameters (`max_records`, `ttl_ledgers`).
   - Verifies caller permission for `module` and `request.scope`.
   - Initializes session in `Active` status with bounded `chunk_size = 25`.
   - Emits session initialization in persistent storage.

2. **`stream_export_chunk(env, caller, session_id, records) -> StreamChunkResult`**
   - Enforces memory limit: `records.len() <= chunk_size` and `<= 50`.
   - Checks that session is `Active` and not expired (`now < expires_ledger`).
   - Rejects if session was cancelled (`ExportError::ExportCancelled`).
   - Scrubs forbidden fields and redacts against safe-field allow-list.
   - Updates monotonic cursor and cumulative SHA-256 stream digest.

3. **`cancel_export_stream(env, caller, session_id, reason) -> ExportCancellationReport`**
   - Validates that caller is the session generator or holds `Role::Admin`.
   - Transitions session to `Cancelled`.
   - Invariant: Once cancelled, all further chunk processing or finalization attempts are strictly rejected.

4. **`finalize_export_stream(env, caller, session_id) -> ExportArtifact`**
   - Seals the session into a completed `ExportArtifact` with full generation metadata.

---

## 4. Forbidden Fields Specification

The export pipeline enforces an unconditional blacklist against sensitive credentials and secrets prior to allow-list redaction:

### Prohibited Fields Blacklist
- **Cryptographic Secrets**: `priv_key`, `sec_key`, `seed`, `entropy`, `token_sec`, `auth_sig`, `sign_key`, `raw_nonce`
- **Authentication Material**: `password`, `admin_key`, `res_seed`, `ballot_slt`
- **PII & Financial Identifiers**: `ssn`, `tax_id`, `bank_wire`, `card_num`, `raw_pii`, `ip_addr`

### Safe Fields Allow-List
Only these fields may appear in exported records:
- `id`, `type`, `status`, `amount`, `created`, `expiry`, `region`, `role`, `tier`, `version`, `network`, `count`.

---

## 5. Failure Modes & Error Taxonomy

| Error Code | Name | Cause | Operator Remediation |
|---|---|---|---|
| `920` | `ScopeDenied` | Caller lacks the role required for the requested module/scope. | Check role assignment (`has_role`) or reduce scope to `OwnRecords`. |
| `921` | `RecordNotVisible` | A record within the batch is outside caller's visibility. | Verify record ownership or request elevated operational scope. |
| `922` | `ExportTooLarge` | Total records exceeds `max_records` or gas ceiling (10,000). | Paginate export across multiple smaller requests. |
| `924` | `ExportExpired` | Ledger sequence has exceeded `expires_ledger`. | Re-initiate stream with a fresh session and appropriate TTL. |
| `925` | `TtlTooLong` | Requested TTL exceeds maximum allowed retention window. | Bound requested TTL within `MAX_EXPORT_TTL_LEDGERS` (180 days). |
| `928` | `ExportCancelled` | Attempted chunk stream or finalization on a cancelled stream. | Cancelled streams cannot be resumed. Initiate a new session. |
| `929` | `ForbiddenFieldDetected` | Prohibited secret or PII field detected under strict mode. | Strip prohibited fields from the producer side before exporting. |
| `930` | `SessionNotFound` | Session ID does not exist in contract persistent storage. | Verify session ID parameter or check if session was pruned. |
| `931` | `ChunkTooLarge` | Submitted chunk exceeds bounded memory limit (50 items). | Slice input records into chunks of 25 items or fewer. |
| `932` | `SessionNotActive` | Operation attempted on completed or failed session. | Inspect session status; completed sessions are sealed. |

---

## 6. Operational Edge Cases & Recovery Runbooks

### Runbook 1: Emergency Stream Cancellation
**Trigger**: A long-running export is consuming excessive storage, or unvetted operational data is being requested.
1. Identify the active session ID.
2. An administrator or the initiating operator calls `cancel_export_stream(&env, &caller, &session_id, &reason)`.
3. Verify the returned `ExportCancellationReport` shows `status: Cancelled`.
4. Subsequent chunk submissions are immediately rejected with error code `928`.

### Runbook 2: Recovering from Interrupted Network Streams
**Trigger**: Client connection drops mid-export after streaming chunk $K$.
1. Query `get_export_session(&env, &session_id)`.
2. Inspect `session.cursor` and `session.status`.
3. If `session.status == Active` and `now < session.expires_ledger`:
   - Resume streaming from record index equal to `session.cursor`.
4. If `session.status == Cancelled` or expired:
   - Discard client-side partial chunks and initialize a fresh session.

### Runbook 3: Secret Leak Incident Response
**Trigger**: A producer improperly attaches private keys or raw PII into record fields.
1. The pipeline's `purge_forbidden_fields` automatically strips forbidden fields before output.
2. If strict rejection is required, the pipeline logs `purged_forbidden_fields` in the session and chunk result.
3. If an export artifact was created prior to discovering upstream contamination:
   - Call `cancel_export_stream` on the session.
   - Invoke retention reclamation to purge the associated `DataClass::Export` record.

---

## 7. Verification

Execute the automated end-to-end verification suite:
```bash
node scripts/verify-export-pipeline.cjs
```
All 6 suites (permissions, chunk boundedness, cancellation, forbidden fields, dataset scalability, and lifecycle) must pass.
