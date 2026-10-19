//! Deterministic Chaos Testing Suite for Interrupted User and Background Workflows (Issue #215).
//!
//! Provides a zero-flakiness, tick-driven chaos scheduling framework that simulates:
//! - Transient network/RPC drops with controlled retry exhaustion and recovery
//! - Mid-execution process crashes during multi-step user and contract workflows
//! - Stale read replica lag and cache invalidation
//! - Background worker restarts and checkpoint resume
//!
//! All faults are seeded with a deterministic pseudo-random number generator (PRNG)
//! without any wall-clock time dependencies (`std::thread::sleep`), ensuring 100%
//! reproducible execution across CI environments.

use soroban_sdk::{contracttype, symbol_short, Env, Symbol, Vec};

/// Deterministic 64-bit XorShift PRNG for reproducible fault injection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeterministicPrng {
    state: u64,
}

impl DeterministicPrng {
    pub fn new(seed: u64) -> Self {
        let state = if seed == 0 { 0x853c49e6748fea9b } else { seed };
        Self { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() & 0xFFFF_FFFF) as u32
    }

    pub fn next_range(&mut self, min: u32, max: u32) -> u32 {
        if min >= max {
            return min;
        }
        let range = max - min + 1;
        min + (self.next_u32() % range)
    }

    pub fn next_bool(&mut self, probability_pct: u32) -> bool {
        self.next_range(1, 100) <= probability_pct
    }
}

/// Simulated chaos fault types.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChaosFault {
    /// Drops the target operation for a specified number of consecutive invocations.
    TransientDrop(Symbol, u32),
    /// Injects replica lag where reads return stale data.
    StaleReadLag(Symbol, u32),
    /// Simulates sudden crash/reboot at a specific execution step.
    MidExecutionCrash(u32),
}

/// Outcome of an intercepted operation under chaos simulation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChaosExecutionResult {
    Success,
    NetworkTimeout,
    StaleReadDetected,
    ProcessCrashed,
}

/// Simulated workflow state for testing interrupted multi-step operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkflowState {
    pub step: u32,
    pub completed: bool,
    pub checkpoint: u32,
    pub processed_items: u32,
    pub ledger_version: u32,
}

/// Deterministic Chaos Scheduler.
#[derive(Clone)]
pub struct DeterministicChaosScheduler {
    pub prng: DeterministicPrng,
    pub current_tick: u32,
    pub faults: Vec<ChaosFault>,
}

impl DeterministicChaosScheduler {
    pub fn new(env: &Env, seed: u64) -> Self {
        Self {
            prng: DeterministicPrng::new(seed),
            current_tick: 0,
            faults: Vec::new(env),
        }
    }

    pub fn advance_tick(&mut self) {
        self.current_tick += 1;
    }

    pub fn schedule_fault(&mut self, fault: ChaosFault) {
        self.faults.push_back(fault);
    }

    /// Intercepts an operation and evaluates active chaos faults deterministically.
    pub fn intercept_op(&mut self, _env: &Env, op: &Symbol) -> Result<(), ChaosExecutionResult> {
        self.advance_tick();

        // 1. Check for mid-execution crashes
        for i in 0..self.faults.len() {
            if let Some(ChaosFault::MidExecutionCrash(at_step)) = self.faults.get(i) {
                if at_step == self.current_tick {
                    return Err(ChaosExecutionResult::ProcessCrashed);
                }
            }
        }

        // 2. Check for transient drops on target operation
        for i in 0..self.faults.len() {
            if let Some(ChaosFault::TransientDrop(target_op, remaining_drops)) = self.faults.get(i) {
                if &target_op == op && remaining_drops > 0 {
                    self.faults.set(i, ChaosFault::TransientDrop(target_op, remaining_drops - 1));
                    return Err(ChaosExecutionResult::NetworkTimeout);
                }
            }
        }

        // 3. Check for stale read replica lag
        for i in 0..self.faults.len() {
            if let Some(ChaosFault::StaleReadLag(target_op, _lag_steps)) = self.faults.get(i) {
                if &target_op == op {
                    return Err(ChaosExecutionResult::StaleReadDetected);
                }
            }
        }

        Ok(())
    }
}

/// Resilient worker executing multi-step workflows with retry and checkpointing.
pub struct ResilientWorkflowWorker {
    pub state: WorkflowState,
    pub max_retries: u32,
}

impl ResilientWorkflowWorker {
    pub fn new(max_retries: u32) -> Self {
        Self {
            state: WorkflowState::default(),
            max_retries,
        }
    }

    /// Executes multi-step batch disbursement with crash recovery and idempotency.
    pub fn execute_batch_disbursement(
        &mut self,
        env: &Env,
        scheduler: &mut DeterministicChaosScheduler,
        total_items: u32,
    ) -> Result<u32, ChaosExecutionResult> {
        let op = symbol_short!("disburse");

        while self.state.processed_items < total_items {
            let mut attempts = 0;
            let mut step_succeeded = false;

            while attempts < self.max_retries {
                attempts += 1;
                match scheduler.intercept_op(env, &op) {
                    Ok(()) => {
                        self.state.processed_items += 1;
                        self.state.step += 1;
                        self.state.checkpoint = self.state.processed_items;
                        step_succeeded = true;
                        break;
                    }
                    Err(ChaosExecutionResult::NetworkTimeout) => {
                        // Simulated backoff without wall-clock sleep
                        scheduler.advance_tick();
                    }
                    Err(ChaosExecutionResult::ProcessCrashed) => {
                        return Err(ChaosExecutionResult::ProcessCrashed);
                    }
                    Err(other) => return Err(other),
                }
            }

            if !step_succeeded {
                return Err(ChaosExecutionResult::NetworkTimeout);
            }
        }

        self.state.completed = true;
        Ok(self.state.processed_items)
    }

    /// Recovers worker state after a simulated crash from checkpoint.
    pub fn recover_from_checkpoint(&mut self) {
        self.state.processed_items = self.state.checkpoint;
    }

    /// Reads data guarded by version check against stale replica lag.
    pub fn read_with_replica_guard(
        &self,
        env: &Env,
        scheduler: &mut DeterministicChaosScheduler,
        expected_version: u32,
    ) -> Result<u32, ChaosExecutionResult> {
        let op = symbol_short!("read_rep");
        match scheduler.intercept_op(env, &op) {
            Ok(()) => Ok(self.state.ledger_version),
            Err(ChaosExecutionResult::StaleReadDetected) => {
                if self.state.ledger_version < expected_version {
                    Err(ChaosExecutionResult::StaleReadDetected)
                } else {
                    Ok(self.state.ledger_version)
                }
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_prng_and_scheduler_reproducibility() {
        let mut prng1 = DeterministicPrng::new(42);
        let mut prng2 = DeterministicPrng::new(42);

        for _ in 0..100 {
            assert_eq!(prng1.next_u64(), prng2.next_u64());
            assert_eq!(prng1.next_range(1, 100), prng2.next_range(1, 100));
        }
    }

    #[test]
    fn test_chaos_transient_network_drops_and_bounded_retry() {
        let env = Env::default();
        let mut scheduler = DeterministicChaosScheduler::new(&env, 12345);
        let op = symbol_short!("disburse");

        // Schedule 2 transient network drops for "disburse"
        scheduler.schedule_fault(ChaosFault::TransientDrop(op, 2));

        let mut worker = ResilientWorkflowWorker::new(5); // 5 retries allowed
        let result = worker.execute_batch_disbursement(&env, &mut scheduler, 10);

        // Worker must succeed in processing all 10 items despite 2 transient drops
        assert_eq!(result, Ok(10));
        assert!(worker.state.completed);
        assert_eq!(worker.state.processed_items, 10);
    }

    #[test]
    fn test_chaos_mid_execution_crash_and_idempotent_resume() {
        let env = Env::default();
        let mut scheduler = DeterministicChaosScheduler::new(&env, 999);
        let mut worker = ResilientWorkflowWorker::new(3);

        // Schedule a process crash at tick 4
        scheduler.schedule_fault(ChaosFault::MidExecutionCrash(4));

        let result = worker.execute_batch_disbursement(&env, &mut scheduler, 10);
        assert_eq!(result, Err(ChaosExecutionResult::ProcessCrashed));

        // State was checkpointed before the crash
        let saved_checkpoint = worker.state.checkpoint;
        assert!(saved_checkpoint > 0 && saved_checkpoint < 10);

        // Simulate reboot / restart: recover from checkpoint and continue
        worker.recover_from_checkpoint();
        scheduler.faults = Vec::new(&env); // Outage cleared after restart

        let resumed_result = worker.execute_batch_disbursement(&env, &mut scheduler, 10);
        assert_eq!(resumed_result, Ok(10));
        assert_eq!(worker.state.processed_items, 10);
        assert!(worker.state.completed);
    }

    #[test]
    fn test_chaos_stale_read_replica_lag_and_cache_invalidation() {
        let env = Env::default();
        let mut scheduler = DeterministicChaosScheduler::new(&env, 777);
        let op = symbol_short!("read_rep");

        scheduler.schedule_fault(ChaosFault::StaleReadLag(op, 5));

        let mut worker = ResilientWorkflowWorker::new(3);
        worker.state.ledger_version = 100;

        // Reading with expected version 105 fails due to replica lag
        let res = worker.read_with_replica_guard(&env, &mut scheduler, 105);
        assert_eq!(res, Err(ChaosExecutionResult::StaleReadDetected));

        // When updated to fresh version, read succeeds
        worker.state.ledger_version = 105;
        let fresh_res = worker.read_with_replica_guard(&env, &mut scheduler, 105);
        assert_eq!(fresh_res, Ok(105));
    }

    #[test]
    fn test_chaos_background_worker_checkpoint_recovery() {
        let mut worker = ResilientWorkflowWorker::new(3);
        worker.state.processed_items = 5;
        worker.state.checkpoint = 5;

        // Simulate failure mutating non-checkpointed ephemeral state
        worker.state.processed_items = 7; // corrupted in-memory state

        // Recovery restores exact checkpoint
        worker.recover_from_checkpoint();
        assert_eq!(worker.state.processed_items, 5);
    }
}
