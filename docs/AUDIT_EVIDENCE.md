# Privacy-Preserving Audit Evidence Bundles

## Overview

During customer support investigations, dispute resolution, or regulatory audits across Trellis contracts (Aid, NFT Marketplace, Treasury, Governance), support engineers require verifiable proof of actions taken without exposing sensitive user identities, personally identifiable information (PII), or private metadata.

The **Audit Evidence Bundle** module (`shared::evidence`) provides an exportable, tamper-evident bundle mechanism that redacts secrets and PII while preserving cryptographic hashes, timestamps, module identifiers, and correlation IDs necessary for independent verification.

---

## Architectural Design

### Core Structures

- **`EvidenceRecord`**: Represents an individual atomic action or event:
  - `record_id: u64`: Monotonically increasing record index.
  - `timestamp: u64`: Ledger timestamp when the operation occurred.
  - `actor_digest: BytesN<32>`: Keyed cryptographic pseudonym of the invoking account.
  - `module: Symbol`: Contract identifier (e.g., `aid`, `nft_mkt`, `trs`).
  - `action: Symbol`: Action executed (e.g., `create`, `claim`, `bid`, `settle`).
  - `correlation_id: BytesN<32>`: Distributed tracing identifier linking workflow spans.
  - `payload_hash: BytesN<32>`: Cryptographic digest of operation parameters with sensitive fields salted and hashed.
  - `status_code: u32`: Execution outcome status (0 for success, non-zero for contract error code).
  - `integrity_hash: BytesN<32>`: SHA-256 fingerprint over canonical record fields.

- **`AuditEvidenceBundle`**: The exportable envelope:
  - `bundle_id: u64`: Unique bundle identifier.
  - `created_at: u64`: Timestamp when the bundle was compiled.
  - `ticket_id: Symbol`: Support investigation ticket reference.
  - `redaction_level: u32`: Applied redaction policy.
  - `record_count: u32`: Total number of records included.
  - `root_digest: BytesN<32>`: Aggregate cryptographic root verifying all records.
  - `records: Vec<EvidenceRecord>`: Bounded collection of redacted evidence records.

---

## Redaction Levels

| Level | Constant | Actor Pseudonymization | Payload Treatment | Permitted Audience |
|---|---|---|---|---|
| **None** | `REDACTION_NONE = 0` | Direct SHA-256 digest of account address | Salted SHA-256 fingerprint | Internal core developers / automated validation |
| **Support** | `REDACTION_SUPPORT = 1` | Keyed salt pseudonymization (`sha256(addr || salt)`) | Salted SHA-256 fingerprint | Tier-2/3 Support engineers & investigators |
| **Strict** | `REDACTION_STRICT = 2` | Maximum isolation with ticket-scoped salt | Fully opaque salted SHA-256 | External auditors & public dispute resolution |

---

## Cryptographic Verification

1. **Per-Record Integrity**:
   Each `EvidenceRecord` contains an `integrity_hash` computed over:
   ```rust
   integrity_hash = sha256(to_xdr([
       record_id, timestamp, actor_digest, module, action, correlation_id, payload_hash, status_code
   ]))
   ```
2. **Bundle Root Verification**:
   The `root_digest` aggregates all `integrity_hash` values sequentially:
   ```rust
   root_digest = sha256(concat(records[0].integrity_hash, ..., records[n-1].integrity_hash))
   ```
3. **Tamper Detection**:
   If an actor, timestamp, payload hash, or status code is modified, `verify_bundle` returns `false` due to hash mismatch.

---

## Operational Edge Cases & Failure Modes

1. **Empty Bundles**: Handled safely with `record_count = 0` and an all-zero `root_digest`.
2. **Salt Leakage**: If an investigation salt is compromised, an adversary could correlate known addresses with actor digests. Salts must be ephemeral, ticket-specific, and stored exclusively in secure secret vaults.
3. **Payload Verification**: Support teams can verify that a customer-provided receipt matches the evidence bundle by computing `hash_sensitive_payload(receipt, ticket_salt)` and matching it against `payload_hash`.
4. **Storage Quota**: Evidence bundles are designed for export and offline validation or short-lived verification; they do not bloat on-chain instance storage.

---

## Rollback & Recovery Procedures

1. **Compromised Salt**: If an investigation salt is leaked, revoke the bundle ticket in the support ticketing system, generate a new random 32-byte salt, and re-export the bundle.
2. **Verification Mismatch**: If `verify_bundle(&env, &bundle)` fails, reject the bundle immediately as tampered or corrupted in transit. Re-extract the canonical audit records from contract event history and regenerate the bundle.
