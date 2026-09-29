//! Typed feature flags for staged rollout and emergency rollback (Issue #43).
//!
//! Risky behavior changes need to ship behind a switch so maintainers can stage
//! a rollout, compare behavior, and disable a new path without redeploying.
//!
//! ## Design
//!
//! - Flags are a **closed, typed set** ([`FeatureFlag`]) so a typo cannot
//!   silently create a new flag; adding one is a code change.
//! - Each flag stores a [`FeatureFlagConfig`] in instance storage:
//!   a master `enabled` switch plus a `rollout_bps` weight (0–10 000) so a flag
//!   can be canaried to a fraction of callers before full rollout.
//! - **Safe default:** a missing config means *disabled*. Nothing risky ever
//!   runs just because storage was never written.
//! - [`emergency_disable`] turns a flag off and zeroes its rollout weight,
//!   which is the rollback path when a staged behavior misbehaves.
//! - [`require_enabled`] is the server-side guard sensitive paths call before
//!   taking a gated branch.
//!
//! See `docs/FEATURE_FLAGS.md` for rollout, verification, and rollback steps.

use soroban_sdk::{contracterror, contracttype, Env};

use crate::storage::{instance_get, instance_set};

/// `10_000` basis points == 100% rollout.
pub const FULL_ROLLOUT_BPS: u32 = 10_000;

/// Feature flag operation errors (range 960–979).
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum FeatureFlagError {
    /// `rollout_bps` is greater than [`FULL_ROLLOUT_BPS`].
    InvalidRollout = 960,
    /// The requested behavior is disabled (or has not been enabled yet).
    Disabled = 961,
}

/// Branch decision for a gated contract path. A missing or disabled flag must
/// remain on the safe legacy path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateDecision {
    Legacy,
    Enabled,
}

/// Typed set of feature flags. Add a variant to introduce a new switch.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeatureFlag {
    /// Gates the batch executor (`shared::batch`) as a kill switch.
    BatchExecutor,
    /// Allows maintainers to bypass quota enforcement during an incident.
    QuotaBypass,
    /// Gates the V2 escrow release path in `shared::payments`.
    EscrowReleaseV2,
    /// Gates the V2 referral payout path.
    ReferralPayoutV2,
}

/// Stored configuration for a single flag.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureFlagConfig {
    /// Master switch. `false` means the gated behavior never runs.
    pub enabled: bool,
    /// Staged-rollout weight in basis points (0–10 000). `10_000` = everyone.
    pub rollout_bps: u32,
    /// Ledger sequence when the flag was last changed.
    pub updated_at: u32,
}

/// Maintainer-facing status snapshot for one flag.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureFlagStatus {
    /// Effective stored config (safe default when unset).
    pub config: FeatureFlagConfig,
    /// Whether the master switch is on.
    pub enabled_now: bool,
}

/// Storage key for a flag. Private — flags are addressed by [`FeatureFlag`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum FeatureFlagKey {
    Flag(FeatureFlag),
}

fn key(flag: &FeatureFlag) -> FeatureFlagKey {
    FeatureFlagKey::Flag(flag.clone())
}

/// Safe default for an unset flag: disabled, no rollout.
pub fn default_config(env: &Env) -> FeatureFlagConfig {
    FeatureFlagConfig {
        enabled: false,
        rollout_bps: 0,
        updated_at: env.ledger().sequence(),
    }
}

/// Read a flag's config, falling back to the safe (disabled) default.
pub fn get_flag(env: &Env, flag: &FeatureFlag) -> FeatureFlagConfig {
    let stored: Option<FeatureFlagConfig> = instance_get(env, &key(flag));
    stored.unwrap_or_else(|| default_config(env))
}

/// Whether a flag's master switch is on. Missing config -> `false`.
pub fn is_enabled(env: &Env, flag: &FeatureFlag) -> bool {
    get_flag(env, flag).enabled
}

/// Whether this caller falls inside the staged rollout window.
///
/// `bucket_bps` must be a **deterministic** value derived from a stable caller
/// identity (see [`rollout_bucket`]). Callers in the same bucket always get the
/// same answer for a given flag config.
pub fn is_enabled_for(env: &Env, flag: &FeatureFlag, bucket_bps: u32) -> bool {
    let config = get_flag(env, flag);
    if !config.enabled {
        return false;
    }
    bucket_bps < config.rollout_bps.min(FULL_ROLLOUT_BPS)
}

/// Deterministic bucket in `0..FULL_ROLLOUT_BPS` from a stable seed
/// (e.g. the first bytes of a caller address). Never random, so rollout is
/// stable across retries.
pub fn rollout_bucket(seed: u32) -> u32 {
    seed % FULL_ROLLOUT_BPS
}

/// Server-side guard for sensitive, gated behavior.
pub fn require_enabled(env: &Env, flag: &FeatureFlag) -> Result<(), FeatureFlagError> {
    if is_enabled(env, flag) {
        Ok(())
    } else {
        Err(FeatureFlagError::Disabled)
    }
}

/// Select the safe legacy path or the explicitly enabled path.
pub fn decide(env: &Env, flag: &FeatureFlag) -> GateDecision {
    if is_enabled(env, flag) { GateDecision::Enabled } else { GateDecision::Legacy }
}

/// Enable/disable a flag and/or set its staged-rollout weight.
///
/// Disabling a flag always zeroes `rollout_bps` so a later re-enable starts
/// from an explicit rollout decision rather than silently resuming the old
/// canary percentage.
pub fn set_flag(
    env: &Env,
    flag: &FeatureFlag,
    enabled: bool,
    rollout_bps: u32,
) -> Result<FeatureFlagConfig, FeatureFlagError> {
    if rollout_bps > FULL_ROLLOUT_BPS {
        return Err(FeatureFlagError::InvalidRollout);
    }
    let config = FeatureFlagConfig {
        enabled,
        rollout_bps: if enabled { rollout_bps } else { 0 },
        updated_at: env.ledger().sequence(),
    };
    instance_set(env, &key(flag), &config);
    Ok(config)
}

/// Emergency rollback: switch a flag off and zero its rollout weight.
///
/// Idempotent — calling it on an already-disabled flag is safe. The flag record
/// is kept (not deleted) so the audit trail shows the rollback happened.
pub fn emergency_disable(env: &Env, flag: &FeatureFlag) -> FeatureFlagConfig {
    let config = FeatureFlagConfig {
        enabled: false,
        rollout_bps: 0,
        updated_at: env.ledger().sequence(),
    };
    instance_set(env, &key(flag), &config);
    config
}

/// Maintainer diagnostics: effective config + whether the flag is on.
pub fn flag_status(env: &Env, flag: &FeatureFlag) -> FeatureFlagStatus {
    let config = get_flag(env, flag);
    let enabled_now = config.enabled;
    FeatureFlagStatus {
        config,
        enabled_now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_defaults_to_disabled() {
        let env = Env::default();
        let flag = FeatureFlag::QuotaBypass;
        let config = get_flag(&env, &flag);
        assert!(!config.enabled);
        assert_eq!(config.rollout_bps, 0);
        assert!(!is_enabled(&env, &flag));
        assert_eq!(
            require_enabled(&env, &flag),
            Err(FeatureFlagError::Disabled)
        );
    }

    #[test]
    fn staged_rollout_respects_bucket() {
        let env = Env::default();
        let flag = FeatureFlag::EscrowReleaseV2;
        // 25% canary.
        set_flag(&env, &flag, true, 2_500).unwrap();
        assert!(is_enabled(&env, &flag));
        assert!(is_enabled_for(&env, &flag, 0));
        assert!(is_enabled_for(&env, &flag, 2_499));
        assert!(!is_enabled_for(&env, &flag, 2_500));
        assert!(!is_enabled_for(&env, &flag, 9_999));

        // Full rollout: everyone is inside the window.
        set_flag(&env, &flag, true, FULL_ROLLOUT_BPS).unwrap();
        assert!(is_enabled_for(&env, &flag, 0));
        assert!(is_enabled_for(&env, &flag, 9_999));
    }

    #[test]
    fn emergency_disable_rolls_back() {
        let env = Env::default();
        let flag = FeatureFlag::BatchExecutor;
        set_flag(&env, &flag, true, FULL_ROLLOUT_BPS).unwrap();
        assert!(is_enabled(&env, &flag));

        let config = emergency_disable(&env, &flag);
        assert!(!config.enabled);
        assert_eq!(config.rollout_bps, 0);
        assert!(!is_enabled(&env, &flag));
        assert!(!is_enabled_for(&env, &flag, 0));
        // Idempotent.
        assert!(!emergency_disable(&env, &flag).enabled);
    }

    #[test]
    fn invalid_rollout_is_rejected() {
        let env = Env::default();
        let flag = FeatureFlag::ReferralPayoutV2;
        assert_eq!(
            set_flag(&env, &flag, true, FULL_ROLLOUT_BPS + 1),
            Err(FeatureFlagError::InvalidRollout)
        );
        // Disabling zeroes the stored weight (no stale canary resumes).
        let config = set_flag(&env, &flag, false, 2_500).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.rollout_bps, 0);
    }

    #[test]
    fn rollout_bucket_is_deterministic() {
        assert_eq!(rollout_bucket(0), 0);
        assert_eq!(rollout_bucket(10_000), 0);
        assert_eq!(rollout_bucket(12_345), 2_345);
        // Deterministic: same seed, same bucket.
        assert_eq!(rollout_bucket(777), rollout_bucket(777));

        // Status reflects the stored config.
        let env = Env::default();
        let flag = FeatureFlag::QuotaBypass;
        set_flag(&env, &flag, true, 5_000).unwrap();
        let status = flag_status(&env, &flag);
        assert!(status.enabled_now);
        assert_eq!(status.config.rollout_bps, 5_000);
    }

    #[test]
    fn decision_defaults_to_legacy_and_rolls_back_to_legacy() {
        let env = Env::default();
        let flag = FeatureFlag::QuotaBypass;
        assert_eq!(decide(&env, &flag), GateDecision::Legacy);
        set_flag(&env, &flag, true, FULL_ROLLOUT_BPS).unwrap();
        assert_eq!(decide(&env, &flag), GateDecision::Enabled);
        emergency_disable(&env, &flag);
        assert_eq!(decide(&env, &flag), GateDecision::Legacy);
    }
}
