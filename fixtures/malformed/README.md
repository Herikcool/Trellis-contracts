# Malformed External Payload Golden Fixture Corpus

A working corpus of golden fixtures for Trellis Contracts. Each fixture is a checked-in JSON or binary artifact that exercises a malformed external payload or an adversarial record against the on-chain contract validation paths.

The corpus is organized by category so that tests can iterate a directory and assert that each fixture is rejected with the expected error code.

## Directory layout

```
fixtures/malformed/
  invalid-signatures/
  missing-fields/
  oversized-values/
  replays/
  schema-drift/
  manifest.json
  README.md
```

Each fixture file is a single JSON object with the following shape:

```json
{
  "id": "invalid-signatures/001-ed25519-bit-flip",
  "description": "HUman-readable summary of the adversarial condition.",
  "module": "treasury",
  "entrypoint": "submit_claim",
  "expected": {
    "outcome": "reject",
    "error": "InvalidSignature"
  },
  "payload": { }
}
```

The `payload` field is the exact bytes that would be submitted to the contract entrypoint, encoded as JSON where the schema is JSON-native. Binary payloads (e.g. raw XDR envelopes) are stored as sibling `.bin` files and referenced from the JSON via `payload_ref`.

## Categories

### invalid-signatures/

Signature verification failures across all signing domains (treasury claims, aid disbursements, referral registrations, governance votes, oracle reports, marketplace listings).

- Ed25519 signature with a single bit flipped in the signature bytes.
- Signature over the wrong domain separator (cross-domain replay).

- Signature from a key that is not the registered signer for the account.
- Signature with an empty or all-zero byte string.
- Signature with a correct signature but a mutated message body (message / signature mismatch).

### missing-fields/

Payloads that omit a required field from the canonical schema. Each fixture omits exactly one field so the failure is attributable.

- Treasury claim without `amount`.
- Treasury claim without `recipient`.
- Aid disbursement without `beneficiary_id`.
- Referral registration without `referrer`.
- Governance vote without `proposal_id`.
- Oracle report without `timestamp`.
- Marketplace listing without `price_asset`.

### oversized-values/

Payloads that exceed declared bounds on individual fields or on the overall payload size.

- `Amount` exceeding the configured maximum claim amount.
- `Amount` set to `ui128::max` (overflow probe).
- `Symbol` longer than the allowed length.
- `Vec<u32>` with more elements than the allowed cap.
- Nested JSON depth exceeding the decoder limit.
- Raw payload byte length exceeding the entry-point limit.

### replays/

Payloads that are well-formed but must be rejected because they were already consumed or are out of order.

- Same nonce submitted twice for a treasury claim.
- Same claim IDentifier submitted again after successful execution.
- Governance vote replayed from a different ledger window.
- Oracle report with a timestamp older than the last accepted report.
- Referral code redeemed after the referral expiry ledger.

### schema-drift/

Payloads that are structurally valid JSON but do not match the expected schema version or type layout.

- Payload with an unknown additional field that must be rejected by strict decoding.
- Payload with a field type changed from `u64` to `string`.
- Payload with a missing `schema_version` field.
- Payload with a `schema_version` newer than the contract supports.
- Payload with a `schema_version` older than the minimum supported version.
- Payload with a field renamed from `recipient` to `destination`.

## Manifest

`manifest.json` is the index of the corpus. It lists every fixture file with its category, module, entry point, expected outcome, and expected error code. Tests iterate the manifest rather than globbing the filesystem directly so that adding a fixture is a deliberate two-step change.

## Verification

The corpus is executed by the contract test suite. Each fixture is fed to the corresponding entry point and the resulting error is compared against `expected.error`. A fixture that is accepted, or that fails with a different error code, fails the suite.

To run the corpus tests locally:

```
cargo test --package trellis-contracts --test malformed_fixtures
```

To validate the manifest and fixture shape without building the contracts:

```
cargo test --package trellis-contracts --test malformed_manifest_integrity
```

## Operational edge cases and failure modes

- Fixtures are data-only. They must not be executed as code and must not contain active credentials or live network endpoints.
- A fixture that is accidentally accepted by a contract is a security regression and must fail CI immediately.
- A fixture that fails with a different error than expected indicates either a validation order change or a new parse path; the manifest must be updated in the same change that alters the error surface.
- Oversized fixtures are generated from a seeded builder so that the byte length is reproducible and the file size is bounded.
- Schema-drift fixtures are versioned alongside the contract schema; a breaking schema change requires a new fixture and a migration note in the PR description.

## Rollback and recovery

- The corpus is additive. Removing a fixture requires removing its manifest entry in the same commit.
- If a fixture becomes incorrect because the contract legitimately accepts it, the correct rollback is to move it to a documented accepted-case fixture with an explicit migration note, not to delete it silently.
- If the manifest and filesystem diverge, the integrity test fails and the corpus must be regenerated from the manifest before merge.
- Recovery from a bad merge is a revert of the fixture commit; no on-chain state is affected because the corpus is test-only data.

## Compatibility

Adding this corpus does not change any public API, schema, or user-facing behavior. It only adds test data and test coverage. Any change to a contract entry point that alters which fixtures are rejected must include a migration note in the PR description.
