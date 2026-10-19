# Testing & Simulation Module

A comprehensive testing framework for all contracts in the trellis-contracts repository. This module provides mocks, helpers, simulation tools, and fuzzing harnesses to thoroughly test all protocol functionality.

## Overview

The module is organized into 6 main components:
- `mocks/` - Mock implementations of external dependencies (tokens, oracles, registry)
- `helpers/` - Time manipulation, environment setup, and debugging utilities
- `simulation/` - Deterministic transaction simulation and gas profiling
- `fuzzing/` - Fuzzing harnesses for critical modules (Access Control, Payments, Upgradeability)
- `snapshot/` - Contract state snapshot verification helpers
- `examples/` - Example test suites demonstrating usage of all features

## Adding to Your Project

Add the testing module to your contract's `Cargo.toml`:

```toml
[dependencies]
testing = { path = "../testing", features = ["testutils"] }
```

## Quick Start

### Basic Test Environment Setup

```rust
use testing::helpers::*;
use testing::mocks::*;

#[test]
fn my_contract_test() {
    // Create a test environment with 5 additional users
    let mut test_env = TestEnvironment::new(5);
    
    // Create a token for testing
    let (token_addr, token_client, asset_client) = test_env.create_stellar_token("usdc");
    
    // Mint initial balances to all users
    test_env.mint_tokens_to_users(&asset_client, &10_000_000);
    
    // Register your contract
    let my_contract_addr = test_env.register_contract("my_contract", MyContract);
    
    // Get a user
    let alice = test_env.user(0);
    
    // Run your test...
}
```

### Time Manipulation

```rust
use testing::helpers::*;

// Advance ledger sequence by 100 blocks
get_ledger_sequence(&env, 100);

// Advance time by 24 hours (86400 seconds)
advance_ledger_time(&env, 86400);

// Jump to a specific timestamp
set_ledger_timestamp(&env, 1704067200); // 2024-01-01
```

### Mocks for External Dependencies

```rust
use testing::mocks::*;

// Create a mock ERC20-like token
let (token_addr, token_client) = create_mock_token(
    &env, 
    &admin, 
    6,      // decimals
    "USD Coin", 
    "USDC"
);

// Create a mock price oracle
let (oracle_addr, _) = create_mock_oracle(&env, &admin);

// Update price in oracle
let oracle_client = MockOracleClient::new(&env, &oracle_addr);
oracle_client.update_price(&btc_address, &450000000000, &6); // $45,000 with 6 decimals

// Create a mock contract registry
let registry_addr = create_mock_registry(&env, &admin);
```

## Contract State Snapshots & Delta Verification

The `snapshot` module provides reusable helpers for capturing contract state
before and after an operation and asserting on the exact delta. This is the
recommended way to test high-risk operations (balance transfers, ownership
changes, status transitions) because it catches unintended side effects.

### Basic Snapshot Diff

```rust
use testing::snapshot::*;
use soroban_std::Symbol;

// Capture the relevant state before the operation.
let mut before = StateSnapshot::new();
before.balance(Symbol::new(&env, "alice"), 1 000);
before.balance(Symbol::new(&env, "bob"), 500);
before.ownership(Symbol::new(&env, "owner"), Symbol::new(&env, "alice"));

// Run the operation.
client.transfer(&alice, &bob, &250);

// Capture the state after.
let mut after = StateSnapshot::new();
after.balance(Symbol::new(&env, "alice"), 750);
after.balance(Symbol::new(&env, "bob"), 750);
after.ownership(Symbol::new(&env, "owner"), Symbol::new(&env, "alice"));

// Assert the exact delta.
let diff = before.diff(&after);
diff.assert_changed(
    &Symbol::new(&env, "alice"),
    &SNapshotValue::Int(1 000),
    &SnapshotValue::Int(750),
);
diff.assert_changed(
    &Symbol::new(&env, "bob"),
    &SNapshotValue::Int(500),
    &SNapshotValue::Int(750),
);
```

### Convenience Wrapper

`capture_diff` captures the diff across an action without having to manually
construct the after snapshot:

```rust
let diff = capture_difd(
    || take_snapshot(&env, &client),
    || client.transfer(&alice, &bob, &250),
);
diff.assert_changed(
    &Symbol::new(&env, "alice"),
    &SNapshotValue::Int(1 000),
    &SnapshotValue::Int(750),
);
```

### Assertion Helpers

| Helper | Purpose |
| --- | --- |
| `assert_changed(&before, &after, &key, &from, &to)` | Assert a key changed from `from` to `to`. |
| `assert_added(&before, &after, &key, &value)` | Assert a key was added with `value`. |
| `assert_removed(&before, &after, &key)` | Assert a key was removed. |
| `assert_no_change(&before, &after)` | Assert nothing changed. |
| `assert_snapshot_eq(&expected, &actual)` | Assert two snapshots are equal. |

All failures panic with a message that includes the expected and actual
diffs, so unexpected deltas are immediately visible in test output.

### Determinism

Diffs are computed in a deterministic order:

1. Keys present in the after snapshot but not the before snapshot (`Added`)
2. Keys present in the before snapshot but not the after snapshot (`Removed`)
3. Keys present in both with different values (`Changed`)

Two snapshots with the same keys and values will always produce the same
diff description, regardless of the order in which keys were recorded.

## Integration Sandbox Mode

Run the primary workflow locally against fake external services — no
production credentials, real wallets, or irreversible records:

```rust
use testing::sandbox::*;

let mut sandbox = Sandbox::new(&env, SandboxConfig::new(1234)).unwrap();
let asset = soroban_std::Symbol::new(&env, "sandbox_asset");

sandbox.oracle_mut().set_price(&asset, 1_000_000).unwrap();
sandbox.token_mut().mint(&payer, 100_000).unwrap();

let report = sandbox
    .run_primary_workflow(&env, &payer, &payee, &asset, 100_000)
    .unwrap();
assert_eq!(report.fee, 500); // 50 bps

// Or run the whole declared scenario matrix:
for outcome in run_all_fixtures(&env).iter() {
    assert!(outcome.passed, "fixture expectation not met");
}
```

Mainnet is rejected, local runs are loopback-only, key material is refused,
and every adapter response is reproducible from `deterministic_seed`. See
[`docs/SANDBOX.md`](../docs/SANDBOX.md) for guardrails, fixtures, and
limitations.

Run the focused synthetic happy-path monitor with
`cargo run -p testing --bin synthetic-journey-monitor`. It reports workflow
latency and names the failing sandbox adapter if the fund/quote/settle journey
fails. See [`docs/SYNTHETIC_JOURNEY_MONITOR.md`](../docs/SYNTHETIC_JOURNEY_MONITOR.md)
for output fields, scope, and recovery guidance.

## Simulation & Gas Profiling

Run deterministic simulations of complex scenarios:

```rust
use testing::simulation::*;

let mut simulator = DeterministicSimulator::new();

// Execute transactions in simulation
for i in 0..100 {
    simulator.execute_tx::<()>(
        &format!("transfer_{}", i),
        &token_address,
        "transfer",
        &sender,
        (recipient.clone(), amount).into(),
    ).unwrap();
}

// Generate report
let results = simulator.finalize();
results.print_gas_report();
```

## Fuzzing Critical Modules

### Access Control Fuzzing

```rust
use testing::fuzzing::*;

let mut fuzzer = AccessControlFuzzer::new(&env, Some(12345)); // fixed seed for reproducibility
let results = fuzzer.fuzz(&AccessControlFuzzConfig::default());
results.print_summary();
assert!(results.caught_violations == results.unauthorized_attempts);
```

### Payment/Treasury Fuzzing

```rust
let mut fuzzer = PaymentsFuzzer::new(&env, None);
let results = fuzzer.fuzz(&PaymentsFuzzConfig::default());
assert!(results.invariant_violations.is_empty(), "No invariant violations allowed");
```

### Upgradeability Fuzzing

```rust
let mut fuzzer = UpgradeabilityFuzzer::new(&env, Some(98765));
let results = fuzzer.fuzz(&UpgradeabilityFuzzConfig::default());
assert_eq(results.unauthorized_attempts_blocked, results.failed_attempts);
```

## Fault Injection

`fault_injection.rs` proves the sandbox's fake dependencies fail the way a
real dependency does — an actionable `Err`, not a panic — and that a failed
call never leaves a partial write behind (issue #127). It covers:

- `Oracle`: a hard failure (`FakeOracleAdapter::fail_for`) and a stale
  price (`mark_stale`) are both surfaced on the returned `Result`/`PriceQuote`
  rather than silently degrading.
- `Token`: an insufficient-balance transfer leaves both balances and the
  supply untouched, and a retry after topping up succeeds exactly once.
- `RPC`: a submission timeout (`FakeRpcAdapter::set_fail_next`) does not
  advance the simulated ledger or record the operation, and the retry that
  follows lands exactly once.
- `Cross-dependency`: a workflow where one leg fails after another has
  already succeeded — the successful leg's state is not rolled back or
  double-counted by the later failure.

Run just this suite:

```bash
cd testing
cargo test fault_injection
```

These tests are fully in-process and deterministic (no RPC, no real token
contract, no wall-clock time), so they run in CI on every PR alongside the
rest of this crate's tests and never flake on an external service's
availability.

## Run All Tests

```bash
cd testing
cargo test
```

## CI Integration

The example tests in this module run automatically in CI. Add your contract's tests that depend on this module to ensure they're always tested against the latest testing utilities.

## Best Practices

1. Always use `TestEnvironment` for consistent setup across all tests
2. Use the simulation module for complex multi-step workflows
3. Add fuzzing tests for all critical functions in your contract
4. Profile gas usage of your contract's operations using the GasProfiler
5. Use the mock contracts to isolate your contract's logic during testing
6. Use snapshot helpers to assert on the exact delta of high-risk operations
7. Use sandbox mode when a test needs an oracle, token, or RPC round-trip:
   the fakes are deterministic, so failures are reproducible

### Multi-actor concurrency schedules

The concurrency::MultiActorConcurrencyHarness runs a deterministic sequence of
actor attempts against the same live test state. Each attempt carries a request
ID, expected revision, and retry number. Tests can arrange stale reads,
duplicate submissions, and retry storms, then assert contract state after each
interleaving. Soroban serializes ledger transactions, so the harness models
submission ordering and stale client snapshots rather than parallel execution.

The treasury scheduled-withdrawal tests cover competing actors, a replayed
request after success, and an expired action:

    cargo test -p testing concurrency

If a sequence fails, compare the attempt order and expected revision with the
contract snapshot. A rejected or expired scheduled withdrawal remains
unexecuted; schedule a new action with a fresh identifier and a valid time
window before retrying. A duplicate after a successful scheduled withdrawal
returns NotFound because success consumes the schedule.
