# Feature Flags — Staged Rollout & Emergency Rollback (Issue #43)

Flags for risky behavior changes in the Trellis Soroban contracts. The registry
lives in [`shared::feature_flags`](../shared/src/feature_flags.rs).

## Model

| Field | Meaning |
|---|---|
| `enabled` | Master switch. `false` → the gated behavior never runs. |
| `rollout_bps` | Staged-rollout weight, `0`–`10_000`. `10_000` = everyone. |
| `updated_at` | Ledger sequence of the last change. |

Flags are a **closed, typed set** ([`FeatureFlag`]) — adding a switch is a code
change, so a typo cannot silently create a new one.

**Safe default:** a flag that has never been written is *disabled* with a
`rollout_bps` of `0`. Nothing risky runs because storage was unset.

## Usage

```rust
use shared::feature_flags::{FeatureFlag, require_enabled, is_enabled_for, rollout_bucket};

// Master-switch guard on a sensitive path.
require_enabled(&env, &FeatureFlag::EscrowReleaseV2)?;

// Staged rollout: deterministic per-caller bucket.
let bucket = rollout_bucket(seed_from_caller);
if is_enabled_for(&env, &FeatureFlag::EscrowReleaseV2, bucket) {
    // new path
} else {
    // existing, safer path
}
```

## Rollout procedure

1. **Stage** at a canary weight, e.g. 5%:

   ```rust
   set_flag(&env, &FeatureFlag::EscrowReleaseV2, true, 500)?;
   ```

2. **Verify** on the canary: watch contract events, error rates, and the
   migration/reconciliation dashboards. Increase
   `rollout_bps` in steps (500 → 2 500 → 10 000).
3. **Full rollout:** `set_flag(&env, &flag, true, FULL_ROLLOUT_BPS)`.

`rollout_bucket(seed)` is deterministic, so a caller that is inside the window
stays inside it across retries — no flip-flopping.

## Emergency rollback

```rust
emergency_disable(&env, &FeatureFlag::EscrowReleaseV2);
```

`emergency_disable` sets `enabled = false` and `rollout_bps = 0`, and keeps the
record (rather than deleting it) so the audit trail shows the rollback. It is
idempotent. The gated path immediately falls back to the existing behavior; no
redeploy is required.

Disabling a flag always zeroes the rollout weight, so a later re-enable starts
from an explicit rollout decision instead of silently resuming the old canary
percentage.

## Applied to a high-risk behavior

`shared::quota::check_and_consume` consults `FeatureFlag::QuotaBypass` before
enforcing limits:

```rust
if crate::feature_flags::is_enabled(env, &FeatureFlag::QuotaBypass) {
    return Ok(get_usage(env, actor, resource)); // incident bypass
}
```

A missing flag config means enforcement stays on (the safer behavior). The
default (disabled) state is covered by the existing quota tests plus the new
`feature_flag_bypass_skips_enforcement_and_rolls_back` test.

The helper returns an explicit `GateDecision`: `Legacy` for a missing or
disabled flag and `Enabled` only after an administrator has written an enabled
configuration. Contract paths should switch on this decision rather than
treating a missing storage value as enabled. The rollback test verifies the
decision returns to `Legacy` after `emergency_disable`.

## Validation

```bash
cargo test -p shared feature_flags
cargo test -p shared quota
```
