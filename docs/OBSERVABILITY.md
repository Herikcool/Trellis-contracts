# Observability

Trellis emits structured **telemetry events** from every critical contract
path. Off-chain indexers turn those events into latency, failure-rate, and
business-conversion dashboards without reading contract storage. This document
defines the payload, the metric names, and ready-to-use dashboard queries.

## High-cardinality protection

Telemetry is deliberately low-cardinality and value-free, but the indexer still
receives untrusted input from the network. A malicious contract or a bug in a
caller can emit telemetry with arbitrary operation symbols, correlation ids,
or actor types. Without guards this blows up metric cardinality, causing
Prometheus memory exhaustion and dashboard timeouts. The shared library
therefore applies three layers of protection before any label is exported:

1. **Allowlist enforcement** — only symbols in `shared::telemetry::CORE_OPERATIONS`
(and the extension allowlist below) are accepted as metric labels. Anything else
is collapsed to the sentinel operation `other`.

2. **Redaction** — label values are normalized and truncated before being
exported. Addresses, amounts, balances, token identifiers, and free-form text
are never included in a label. The correlation id is the only caller-supplied
value that survives, and it is only exposed through the controlled `correlation_id`
dimension (cardinality-bounded, see below).

3. **Sampling** — when a single operation exceeds the configured per-operation
rate limit, excess events are dropped from the counter stream and accounted in
the `trellis_telemetry_sampled_out_total` counter instead. The histogram and
failure counters are never sampled out, so failure detection remains exact.

All three layers are implemented in `shared::telemetry::sanitize` and are
covered by `shared::test_telemetry::sanitization_tests`.

### Allowlist

| Operation | Symbol | Source |
| --- | --- | --- |
| Payment transfer | `pay_xfr` | core |
| Escrow creation | `esc_crt` | core |
| Escrow release | `esc_rel` | core |
| Quota consumption | `quota_csm` | core |
| Strategy rebalance | `rebal` | core |
| Aid creation | `aid_crt` | extension |
| Aid claim | `aid_clm` | extension |
| Referral registration | `ref_reg | extension |
| Governance proposal | `gov_prp` | extension |
| Governance vote | `gov_vt` | extension |
| Oracle update | `orc_upd` | extension |
| Marketplace listing | `mkt_lst` | extension |
| Marketplace sale | `mkt_sale` | extension |

Symbols not in this table are rejected and counted under `operation="other"`.
The core list is exposed as `shared::telemetry::CORE_OPERATIONS` and the
extension list as `shared::telemetry::EXTENSION_OPERATIONS`.

### Redaction rules

Label values are sanitized as follows:

- **Operation**: must match `^[a-z][a-z0-9_]{0,15}$` and be in the allowlist.
Otherwise it is replaced with `other`. This bounds the operation label to at
most `|CORE_OPERATIONS| + |EXTENSION_OPERATIONS| + 1` values.
- **Actor*: must be one of `User`, `Admin`, `Contract`, `Worker`, `System`.
Anything else is redacted to `System`.
- **Result**: must be `Success` or `Failure`. Anything else is redacted to
`Failure` (so an invalid result is never silently counted as a success).
- **Correlation id**: never exported as a label. It is hashed into a fixed
bucket of 1024 values and exposed as `correlation_id_bucket="0-1023"`. The
raw id remains available in the indexed event table for tracing but is never
used as a metric label.
- **Free-form text**: not accepted. The `TelemetryEvent` schema has no
string field besides the operation symbol, so there is nowhere for addresses or
amounts to leak into a label.

### Sampling rules

- Every operation has a per-ledge budget of `TELEMETRY_PER_OP_PER_LEDGER`
(default 1000). The first budget events per operation per ledger are
exported in full.
- Events beyond the budget increment `trellis_telemetry_sampled_out_total`
with the same `operation` label and are not added to `trellis_operation_total`.
- **Exemptions:** `trellis_operation_failure_total` and
`trellis_operation_latency_ledgers` are never sampled out. A surge of failures is
exactly when operators need complete data, and the histogram is bounded by the
number of latency buckets rather than the number of events.
- Sampling is applied per ledger, not per wall clock, so the admission rate is
deterministic and replayable from the ledger stream.
- When the budget is exceeded, a single `trellis_telemetry_sample_overflow`
gauge is set to `1` for the affected operation so dashboards can show "this
panel is now an undercount" instead of silently dropping data.

The admission budget is configurable without a contract upgrade via the
indexer environment variable `TELEMETRY_PER_OPC_PER_LEDGER`. Setting it to `0`
disables sampling entirely (not recommended in production).

## Operational Health Indicators

The `shared::dashboard` module aggregates key indicators to track operational health:
- **Unresolved Failures & Stale Jobs**: Derived from background worker dead letters (failed beyond retry limits).
- **Reconciliation Drift**: Derived from the reconciliation module, highlighting mismatch between ledger, off-chain DB, and user caches.
- **User-Impacting Incidents**: Derived from dependency health checks where critical services are marked degraded or down.

These can be fetched programmatically via the `generate_dashboard` method, which redacts sensitive information such as specific user addresses.

## Event contract

Every telemetry event is published with:

- **Topics:* `(tel, operation)` — filter on the first topic to subscribe to
  telemetry only.
- **Data:** a `TelemetryEvent` with a fixed field set.

| Field | Type | Meaning |
| --- | --- | --- |
| `operation` | `Symbol` | Low-cardinality operation name (see the table below). |
| `actor` | `ActorType` | `User`, `Admin`, `Contract`, `Worker`, or `System`. |
| `result` | `TelemetryResult` | `Success` or `Failure`. |
| `latency_ledgers` | `u32` | Ledgers elapsed between start and finish. `0` for single-ledger operations. |
| `correlation_id` | `u64` | Caller-supplied workflow id, or the ledger sequence when the caller supplies none. |

### Core operations

| Operation | Symbol | Instrumented path |
| --- | --- | --- |
| Payment transfer | `pay_xfr` | `shared::payments::safe_transfer` |
| Escrow creation | `esc_crt` | `shared::payments::create_escrow` |
| Escrow release | `esc_rel` | `shared::payments::release_escrow` |
| Quota consumption | `quota_csm` | `shared::quota::check_and_consume` |
| Strategy rebalance | `rebal` | `contracts/rebalancer-contract::rebalance` |

`shared::telemetry::CORE_OPERATIONS` lists these in code, and
`shared::test_telemetry` asserts that each emits a payload carrying all five
structured fields.

### Sensitive values

Telemetry deliberately carries **no addresses, amounts, balances, tokens,
secrets, or free-form text**. Failures are described by the operation name plus
the correlation id; the contract error is still returned to the caller and, for
payment paths, published through the existing typed events. Keeping telemetry
low-cardinality and value-free also keeps it cheap to index.

### Emitting from a new path

```rust
use shared::telemetry::{ActorType, TelemetryTimer, OP_REBALANCE};

let timer = TelemetryTimer::start_here(&env, OP_REBALANCE, ActorType::Contract);
// ... do the work ...
if worked {
    timer.succeed(&env);
} else {
    timer.fail(&env);
}
```

For single-ledger operations where no timer is needed, use
`emit_success(&env, OP, actor, 0)` / `emit_failure(&env, OP, actor, 0)`.

## Metric names

| Metric | Type | Labels | Definition |
| --- | --- | --- | --- |
| `trellis_operation_total` | counter | `operation`, `actor`, `result` | Telemetry payloads observed. |
| `trellis_operation_success_total` | counter | `operation` | Payloads with `result = Success`. |
| `trellis_operation_failure_total` | counter | `operation` | Payloads with `result = Failure`. |
| `trellis_operation_failure_ratio` | gauge | `operation` | `failure_total / total`. |
| `trellis_operation_latency_ledgers` | histogram | `operation` | Distribution of `latency_ledgers`. |
| `trellis_operation_throughput_total` | counter | `operation` | Same source as `trellis_operation_total`; used for rate panels. |
| `trellis_conversion_total` | counter | `funnel_step` | Business-critical conversions (`esc_crt`, `esc_rel`, `pay_xfr`). |
| `trellis_telemetry_sampled_out_total` | counter | `operation` | Events dropped because the per-operation budget was exhausted. |
| `trellis_telemetry_sample_overflow` | gauge | `operation` | `1` when the last ledger exceeded the budget for this operation. |

Map one indexer row per telemetry event:

```text
trellis_operation_total{operation="esc_crt",actor="user",result="success"} 1
```

## Dashboard queries

The queries below assume an indexed table `soroban_events` with one row per
event:

```sql
-- ledger_closed_at TIMESTAMPTZ
-- contract_id      TEXT
-- topic_0          TEXT   -- "tel"
-- operation        TEXT   -- decoded from the payload
-- actor            TEXT
-- result          TEXT   -- "success" | "failure"
-- latency_ledgers  INTEGER
-- correlation_id   BIGINT
```

### Failure rate by operation (last 24h)

```sql
SELECT operation,
       COUNT(*) FILTER (WHERE result = 'failure') AS failures,
       COUNT(*)                                   AS total,
       ROUND(
         COUNT(*) FILTER (WHERE result = 'failure')::numeric
         / NULLIF(COUNT(*), 0), 4
       ) AS failure_ratio
FROM soroban_events
WHERE topic_0 = 'tel'
  AND ledger_closed_at > now() - interval '24 hours'
GROUP BY operation
ORDER BY failure_ratio DESC;
```

### Latency percentiles by operation (last 24h)

```sql
SELECT operation,
       percentile_cont(0.50) WITHIN GROUP (ORDER BY latency_ledgers) AS p50,
       percentile_cont(0.95) WITHIN GROUP (ORDER BY latency_ledgers) AS p95,
       MAX(latency_ledgers)                                          AS worst
FROM soroban_events
WHERE topic_0 = 'tel'
  AND result = 'success'
  AND ledger_closed_at > now() - interval '24 hours'
GROUP BY operation
ORDER BY p95 DESC;
```

### Business conversion funnel (escrow lifecycle, hourly)

```sql
SELECT date_trunc('hour', ledger_closed_at) AS bucket,
       COUNT(*) FILTER (WHERE operation = 'esc_crt' AND result = 'success') AS created,
       COUNT(*) FILTER (WHERE operation = 'esc_rel' AND result = 'success') AS released,
       ROUND(
         COUNT(*) FILTER (WHERE operation = 'esc_rel' AND result = 'success')::numeric
         / NULLIF(COUNT(*) FILTER (WHERE operation = 'esc_crt' AND result = 'success'), 0), 4
       ) AS release_rate
FROM soroban_events
WHERE topic_0 = 'tel'
  AND ledger_closed_at > now() - interval '7 days'
GROUP BY bucket
ORDER BY bucket;
```

### Trace one workflow by correlation id

```sql
SELECT ledger_closed_at, operation, actor, result, latency_ledgers
FROM soroban_events
WHERE correlation_id = 4242
ORDER BY ledger_closed_at;
```

### Where users fail

```sql
SELECT operation, actor, COUNT(*) AS failures
FROM soroban_events
WHERE topic_0 = 'tel'
  AND result = 'failure'
  AND ledger_closed_at > now() - interval '24 hours'
GROUP BY operation, actor
ORDER BY failures DESC;
```

### Sampled-out events by operation (last 24h)

Use this to confirm that a dashboard is not silently undercounting due to
sampling:

```sql
SELECT operation,
       SUM(sampled_out) AS sampled_out,
       SUM(total)        AS admitted,
       ROUND(
         SUM(sampled_out)::numeric
         / NULLIF(SUM(sampled_out) + SUM(total), 0), 4
       ) AS drop_rate
FROM trellis_telemetry_sample_buckets
WHERE bucket > now() - interval '24 hours'
GROUP BY operation
ORDER BY drop_rate DESC;
```

### High-cardinality rejections (last 24h)

When an operation symbol is rejected by the allowlist, it is counted under
`operation="other"`. A sudden spike in this series means a contract is emitting
unrecognized telemetry and should be investigated:

```sql
SELECT date_trunc('minute', ledger_closed_at) AS bucket,
       COUNT(*) AS rejected
FROM soroban_events
WHERE topic_0 = 'tel'
  AND operation = 'other'
  AND ledger_closed_at > now() - interval '24 hours'
GROUP BY bucket
ORDER BY bucket;
```

## Subscribing to the raw stream

```bash
# Stellar RPC: pull telemetry events for a contract from a starting ledger
soroban events \
  --id <CONTRACT_ID> \
  --start-ledger <LEDGER> \
  --network testnet \
  --topic-filter-1 "tel"
```

Indexers should key on `correlation_id` to join telemetry to the contract's
typed events (for example `pay_esc_c` / `pay_esc_r`) when a human-auditable
trail is needed.

## Operational edge cases and failure modes

### Edge cases

- **Unknown operation symbol.** A contract that emits a symbol outside the
allowlist is counted as `operation="other"`. The raw symbol is still present in
the indexed event row, so the offending contract can be identified from
`contract_id`. The metric label never carries the raw symbol.
- **Invalid actor or result.** Redacted to `System` / `Failure` respectively. An
invalid result is treated as a failure so a malformed payload cannot inflate the
success rate.
- **Correlation-id flood.** The correlation id is never a label. It is
bucketed into 1024 values for the optional `correlation_id_bucket` dimension, so
an attacker cannot explode the label space by supplying unique ids.
- **Ledger with many events.** The per-operation budget applies per ledger. A
ledger that contains many telemetry events for the same operation will have its
excess counted in `trellis_telemetry_sampled_out_total`.
- **Missing latency.** Single-ledger operations emit `latency_ledgers
= 0`. The histogram includes a dedicated `0` bucket so these do not distort
percentiles.

### Failure modes

| Failure mode | Signal | Response |
| --- | --- | --- |
| Indexer down | `rate(trellis_operation_total[5m]) == 0` for all operations | Page on complete data loss; check indexer health before contracts. |
| Sampling misconfigured too low | `trellis_telemetry_sample_overflow`=1 sustained | Raise `TELEMETRY_PER_OPC_PER LEDGER` and restart the indexer. |
| Abusive event volume | `trellis_telemetry_sampled_out_total` spikes with `operation="other"` | Identify the offending `contract_id` from the event table and disable or upgrade it. |
| Label cardinality growth | Prometheus target memory growth without event volume growth | Verify the allowlist is still in place; a regular expression change may have bypassed it. |

### Rollback and recovery

- **Rollback sampling.** Set `TELEMETRY_PER_OPC_PER_LEDGER=0` and restart the
indexer. This disables sampling and restores full event admission without a
deploy. Revert to the default once the event volume is understood.
- **Rollback redaction.** Redaction is a compile-time guarantee in `shared::telemetry`.
If a new operation needs to be added, add it to the allowlist and ship a new
contract version. Do not bypass the allowlist in the indexer.
- **Recovery from a dashboard outage.** The raw event table is the source of
truth. Rebuild Prometheus series from `soroban_events` using the queries above;
no data is lost when only the metric pipeline fails.
- **Recovery from a bad operation symbol.** Add the symbol to the
extension allowlist in a new indexer release, or fix the emitting contract. Until
then the events are visible under `operation="other"` and are not lost.

## Contract state snapshots

Contract test suites use the snapshot helpers documented in
[`CONTRACT_STATE_SNAPSHOTS.md`](./CONTRACT_STATE_SNAPSHOTS.md) to capture
deterministic before/after state and assert expected deltas for balances,
ownership, status, and metadata. Unexpected deltas fail tests with a
structured diff of the observed changes.

## Automated budget profiling

Telemetry and snapshots tell you *what* happened to a call. This section is
about *what it cost*: the Soroban host's CPU-instruction and memory-byte
meters, which are the same quantities the network charges for at runtime.
CI profiles these on every pull request so a change that quietly makes a
critical path more expensive is caught before merge, not after a mainnet
budget-exceeded failure.

### What is profiled

The `profile-budget` CI job (`.github/workflows/ci.yml`) measures one
representative call per contract via `Env::budget()`, the same host API
`contracts/treasury-contract/src/budget_test.rs` uses for its per-PR
regression assertions (issue #158):

| Contract | Call | Harness |
| --- | --- | --- |
| `aid-contract` | `create_aid` | `contracts/aid-contract/src/profile_budget.rs` |
| `aid-contract` | `claim_aid` | `contracts/aid-contract/src/profile_budget.rs` |
| `treasury-contract` | `withdraw` | `contracts/treasury-contract/src/profile_budget.rs` |
| `referral-contract` | `register` | `contracts/referral-contract/src/profile_budget.rs` |
| `rebalancer-contract` | `rebalance` | `contracts/rebalancer-contract/src/profile_budget.rs` |

Each harness resets the budget, calls the contract once through a realistic
fixture, and prints the delta as a machine-readable line:

```text
BUDGET_METRIC aid.create_aid cpu=123456 mem=7890
```

`scripts/profile-budget.sh` runs `cargo test -p <contract> --lib
profile_budget -- --nocapture` for each contract above, collects those
lines, and hands them to `scripts/compare-budget.cjs`.

This is a deliberately different mechanism from `budget_test.rs`'s
`assert_within`: that suite pins hard-coded, per-contract limits inside the
crate and asks "did this change make the call meaningfully worse than the
commit that set the constant." The CI job here asks a cheaper, more general
question -- "compared to the last-recorded baseline, by how much, right
now" -- for every profiled contract from one place, without needing a new
hard-coded constant per call.

### Baseline and threshold

Measured costs are compared against the checked-in baseline at
`testing/budget-baseline.json` (`{ "metrics": { "<contract>.<call>":
{
 "cpu_instructions": N, "memory_bytes": M }, ... } }`). A call fails the job
when either its CPU instructions or its memory bytes increase by more than
**10%** relative to the baseline. That threshold is deliberately loose, for the
same reason `budget_test.rs`'s `HEADROOM_PERCENT` is: a `soroban-sdk`
patch bump can shift the host's cost model by a few percent, and that alone
should not fail an unrelated PR. A real regression is typically measured in
multiples, not single-digit percentages.

The threshold is tunable without editing the workflow: set the repository
variable `BUDGET_REGRESSION_THRESHOLD_PERCENT` (Settings → Secrets and
variables → Actions → Variables), or pass `--threshold <percent>` when
running `scripts/profile-budget.sh` locally. It defaults to `10`.

### Updating the baseline

When a measured increase is intentional (a new guard, a richer event
payload, an added storage read), regenerate the baseline and explain why in the PR:

```bash
./scripts/profile-budget.sh --update-baseline
git add testing/budget-baseline.json
```

### Reading the job output

profile-budget writes a markdown comparison table to the job's
$GITHUB_STEP_SUMMARY (visible on the job's summary page, no PR comment
needed) with one row per profiled call:

- **PASS** — within the threshold.
- **FAIL** — CPU or memory regressed beyond the threshold; the job exits
  non-zero.
- **NEW** — measured this run but absent from the baseline (a call profiled
  for the first time); does not fail the job.
- **MISSING** — present in the baseline but not measured this run, because
  that contract's `profile_budget` tests did not build or run. This is
  reported as a warning, not a job failure, so an unrelated, pre-existing
  problem in one contract's test suite cannot silently block every other
  contract's profiling.

### Running it locally

```bash
./scripts/profile-budget.sh                        # measure and compare
./scripts/profile-budget.sh --threshold 25          # loosen the gate for one run
/scripts/profile-budget.sh --update-baseline       # regenerate the baseline file
```

Or, to inspect a single contract's raw measurement without the comparison
step:

```bash
cargo test -p aid-contract --lib profile_budget -- --nocapture --test-thread
```
