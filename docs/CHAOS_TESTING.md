# Deterministic Chaos Testing Suite

## Overview

Distributed contract systems and off-chain background workers often fail under non-ideal real-world conditions: transient RPC drops, read-replica replication lag, mid-execution container crashes, and ledger reorgs.

Traditional chaos suites frequently suffer from **flaky timing assumptions** and race conditions caused by wall-clock sleeps (`std::thread::sleep`). The **Deterministic Chaos Testing Suite** (`shared::chaos`) solves this problem by using discrete tick scheduling and a seeded pseudo-random number generator (PRNG). Every test scenario is 100% reproducible across different machines and CI runs.

---

## Architectural Components

### 1. `DeterministicPrng`
A lightweight 64-bit XorShift pseudo-random number generator.
- Completely platform-independent.
- Given an identical seed, produces the exact same sequence of pseudo-random numbers, enabling precise failure injection.

### 2. `DeterministicChaosScheduler`
A tick-driven interception engine:
- Advances discrete logical ticks on every operation intercept (`scheduler.intercept_op(&op)`).
- Eliminates non-deterministic wall-clock timing.
- Maintains a registry of scheduled `ChaosFault` entries.

### 3. Fault Primitives (`ChaosFault`)

| Fault Type | Description | System Target |
|---|---|---|
| **`TransientDrop`** | Simulates dropped network packets or RPC timeouts for $N$ consecutive attempts before self-clearing. | Public RPC endpoints, relayer submissions, external oracles. |
| **`StaleReadLag`** | Simulates replica lag where read replicas return ledger state from earlier ledgers. | Discovery queries, pagination indexes, balance readers. |
| **`MidExecutionCrash`** | Simulates sudden process death, OOM, or node reboot at an exact execution tick. | Multi-step batch disbursement, auction settlement, migration loops. |

---

## Simulated Workflow Patterns

### 1. Resilient Batch Disbursement
When executing multi-step batch operations:
- The worker records a durable `checkpoint` after each successfully processed item.
- If a `MidExecutionCrash` occurs at tick $T$, the worker terminates immediately with `ProcessCrashed`.
- On restart, `worker.recover_from_checkpoint()` restores the exact progress, and execution resumes idempotently without double-spending or duplicate token transfers.

### 2. Transient Drops with Discrete Backoff
- When an operation encounters `NetworkTimeout`, the worker retries up to `max_retries` times.
- Each retry advances the scheduler tick (discrete backoff).
- Once the configured `remaining_drops` count reaches zero, the network recovers and the operation completes.

### 3. Stale Replica Lag Detection
- In distributed Stellar node topologies, read queries may hit a follower node that is behind the validator head.
- The `read_with_replica_guard` pattern checks the returned snapshot version against the expected ledger sequence.
- If staleness is detected (`StaleReadDetected`), the client invalidates the local cache and routes the read to the authoritative ledger, guaranteeing strong consistency.

---

## Operational Runbook & Edge Cases

### Edge Cases
1. **Retry Exhaustion**: If transient outages exceed `max_retries`, the worker returns `NetworkTimeout` without corrupting state or skipping items.
2. **Crash Before First Checkpoint**: If the worker crashes before the first item completes, state remains at 0 items; no partial transaction is committed.
3. **Repeated Crashes**: The scheduler can inject repeated crashes at sequential steps; the worker continues to advance incrementally from the latest checkpoint.

### Recovery Procedures
1. **Worker Crash Recovery**:
   - Restart the worker process.
   - Read the last committed checkpoint from contract storage.
   - Resume processing from index `checkpoint + 1`.
2. **Replica Lag Recovery**:
   - On `StaleReadDetected`, pause read traffic to the lagged replica.
   - Force a cache-busting read directly against the validator node or wait for the replica sequence number to catch up.
