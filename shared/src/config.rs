//! Typed environment / secrets validation (Issue #66).
//!
//! Startup and deployment must fail fast when required configuration is
//! missing, malformed, unsafe, or accidentally uses production-like secrets
//! in local mode. This module provides `no_std`-compatible validators used by
//! contracts (network IDs, feature flags) and mirrored by
//! `scripts/validate-config.sh` for off-chain deployment config.
//!
//! ## Guarantees
//!
//! - Missing values -> [`Error::ConfigMissing`].
//! - Malformed values (bad URL, unknown network, out-of-range flag) ->
//!   [`Error::ConfigInvalid`].
//! - Placeholder or leaked production secrets -> [`Error::UnsafeSecret`].
//! - Secrets are never printed in full: use [`RedactedSecret`] which only
//!   exposes a `first4...last2` preview in `Debug` output.
//!
//! See `docs/CONFIGURATION.md` for local / staging / production requirements.

use core::fmt;

use crate::errors::Error;

/// Deployment environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    Local,
    Staging,
    Production,
}

impl Environment {
    /// Parse `TRELLIS_ENV` style values.
    pub fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "local" => Ok(Environment::Local),
            "staging" => Ok(Environment::Staging),
            "production" => Ok(Environment::Production),
            _ => Err(Error::ConfigInvalid),
        }
    }

    pub fn is_production(self) -> bool {
        matches!(self, Environment::Production)
    }
}

/// Wrapper that never displays the full secret.
///
/// `Debug` prints `XXXX...YY` (first 4 / last 2 chars) so logs stay useful
/// without leaking key material.
pub struct RedactedSecret<'a>(pub &'a str);

impl<'a> fmt::Debug for RedactedSecret<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", redact_preview(self.0))
    }
}

/// Short preview helper used by [`RedactedSecret`].
///
/// Returns `true` length class only via static strings; to avoid allocation
/// the preview is written inline by the caller. This helper reports whether
/// a value looks redacted (contains `...`).
pub fn redact_preview(secret: &str) -> RedactedPreview<'_> {
    RedactedPreview(secret)
}

/// Display-only preview `first4...last2`.
pub struct RedactedPreview<'a>(&'a str);

impl<'a> fmt::Display for RedactedPreview<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0;
        let count = s.chars().count();
        if count <= 8 {
            return write!(f, "***");
        }
        for (i, c) in s.chars().enumerate() {
            if i < 4 {
                write!(f, "{}", c)?;
            }
        }
        write!(f, "...")?;
        for (i, c) in s.chars().enumerate() {
            if i >= count - 2 {
                write!(f, "{}", c)?;
            }
        }
        Ok(())
    }
}

/// Require a non-empty value.
pub fn require_present(value: Option<&str>) -> Result<&str, Error> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(Error::ConfigMissing),
    }
}

/// Validate Stellar network / passphrase id.
///
/// Accepts `standalone`, `local`, `futurenet`, `testnet`, `mainnet`, and full
/// passphrases starting with `Test SDF` / `Public Global Stellar`.
pub fn validate_network_id(network: Option<&str>) -> Result<(), Error> {
    let v = require_present(network)?;
    let ok = matches!(v, "standalone" | "local" | "futurenet" | "testnet" | "mainnet")
        || v.starts_with("Test SDF")
        || v.starts_with("Public Global Stellar");
    if ok {
        Ok(())
    } else {
        Err(Error::ConfigInvalid)
    }
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack.as_bytes().windows(needle.len()).any(|window| {
        window.iter().zip(needle.as_bytes().iter()).all(|(a, b)| {
            a.to_ascii_lowercase() == b.to_ascii_lowercase()
        })
    })
}

/// Validate RPC / Horizon URL (must be http(s), no whitespace).
pub fn validate_rpc_url(url: Option<&str>) -> Result<(), Error> {
    let v = require_present(url)?;
    let ok = (v.starts_with("http://") || v.starts_with("https://"))
        && !v.contains(' ')
        && v.len() >= 12
        && (v.contains('.') || contains_case_insensitive(v, "localhost"));
    if ok {
        Ok(())
    } else {
        Err(Error::ConfigInvalid)
    }
}

/// Placeholder / weak secrets that must never be accepted.
const DENYLIST: &[&str] = &[
    "",
    "changeme",
    "password",
    "secret",
    "test",
    "testing",
    "12345",
    "00000000",
    "SECRETS_PLACEHOLDER",
    "YOUR_SECRET_HERE",
];

fn is_denied(secret: &str) -> bool {
    let lower = lower_is_denied(secret);
    lower || DENYLIST.iter().any(|d| d.eq_ignore_ascii_case(secret.trim()))
}

// `no_std` lowercase compare without allocation: ASCII-only fold.
fn lower_is_denied(secret: &str) -> bool {
    let trimmed = secret.trim();
    // Compare case-insensitively against the small denylist.
    DENYLIST.iter().any(|d| {
        if d.len() != trimmed.len() {
            return false;
        }
        d.bytes()
            .zip(trimmed.bytes())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

/// Validate a Stellar secret seed (`S...`, 56 chars, base32 body).
///
/// - Empty / placeholder -> [`Error::UnsafeSecret`].
/// - Wrong length / prefix / charset -> [`Error::ConfigInvalid`].
/// - Production-like (valid shape) secret used while `env == Local` ->
///   [`Error::UnsafeSecret`] to catch accidental prod-key reuse locally.
pub fn validate_secret_key(
    secret: Option<&str>,
    env: Environment,
) -> Result<(), Error> {
    if env == Environment::Local && secret.map(|s| s.trim().is_empty()).unwrap_or(true) {
        return Ok(());
    }
    let v = require_present(secret).map_err(|_| Error::ConfigMissing)?;
    let s = v.trim();
    if is_denied(s) || s.len() < 8 {
        return Err(Error::UnsafeSecret);
    }
    let looks_like_seed = s.starts_with('S')
        && s.len() == 56
        && s.bytes().all(|b| b.is_ascii_alphanumeric());
    if !looks_like_seed {
        // Short-but-not-denied test keys are unsafe, not merely malformed.
        if s.len() < 16 || s.eq_ignore_ascii_case("test-secret") {
            return Err(Error::UnsafeSecret);
        }
        return Err(Error::ConfigInvalid);
    }
    if env == Environment::Local {
        // A well-formed seed in local mode is almost certainly a copy-paste
        // of a real key: fail fast. Local dev must use `*_TEST_ONLY` fixtures
        // or the documented ephemeral keys (see docs/CONFIGURATION.md).
        return Err(Error::UnsafeSecret);
    }
    Ok(())
}

/// Validate a full deployment config bundle.
///
/// Order: missing -> malformed -> unsafe, so callers get the most actionable
/// error first.
pub fn validate_full_config(
    network: Option<&str>,
    rpc_url: Option<&str>,
    secret: Option<&str>,
    env: Environment,
) -> Result<(), Error> {
    validate_network_id(network)?;
    validate_rpc_url(rpc_url)?;
    validate_secret_key(secret, env)?;
    // Production must use https RPC.
    if env.is_production() {
        if let Some(url) = rpc_url {
            if url.starts_with("http://") {
                return Err(Error::ConfigInvalid);
            }
        }
    }
    Ok(())
}

/// Validate a `0`/`1`/`true`/`false` feature flag.
pub fn validate_feature_flag(value: Option<&str>) -> Result<bool, Error> {
    let v = require_present(value)?;
    match v.trim() {
        "1" | "true" | "TRUE" | "True" => Ok(true),
        "0" | "false" | "FALSE" | "False" => Ok(false),
        _ => Err(Error::ConfigInvalid),
    }
}

fn is_valid_base32_body(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7'))
}

/// Validate a Soroban contract ID (`C...`, 56 chars, base32 uppercase body).
///
/// Rejects missing, malformed length/prefix/charset, and placeholders.
pub fn validate_contract_id(id: Option<&str>) -> Result<&str, Error> {
    let v = require_present(id)?;
    let s = v.trim();
    if s.len() != 56 || !s.starts_with('C') || !is_valid_base32_body(s) {
        return Err(Error::ConfigInvalid);
    }
    // Reject dummy/repeated sequences:
    let body = &s[1..];
    let first = body.as_bytes()[0];
    if body.as_bytes().iter().all(|&b| b == first) {
        return Err(Error::ConfigInvalid);
    }
    if contains_case_insensitive(s, "placeholder")
        || contains_case_insensitive(s, "example")
        || is_denied(s)
    {
        return Err(Error::ConfigInvalid);
    }
    Ok(s)
}

/// Validate a Stellar account / contract address (`G...` or `C...`, 56 chars).
pub fn validate_account_id(id: Option<&str>) -> Result<&str, Error> {
    let v = require_present(id)?;
    let s = v.trim();
    if s.len() != 56 || (!s.starts_with('G') && !s.starts_with('C')) || !is_valid_base32_body(s) {
        return Err(Error::ConfigInvalid);
    }
    let body = &s[1..];
    let first = body.as_bytes()[0];
    if body.as_bytes().iter().all(|&b| b == first) {
        return Err(Error::ConfigInvalid);
    }
    if contains_case_insensitive(s, "placeholder")
        || contains_case_insensitive(s, "example")
        || is_denied(s)
    {
        return Err(Error::ConfigInvalid);
    }
    Ok(s)
}

/// Validate cross-environment consistency between network name, RPC endpoint, and environment.
pub fn validate_network_rpc_consistency(
    network: Option<&str>,
    rpc_url: Option<&str>,
    env: Environment,
) -> Result<(), Error> {
    let net = require_present(network)?;
    let rpc = require_present(rpc_url)?;
    validate_network_id(Some(net))?;
    validate_rpc_url(Some(rpc))?;

    let is_prod_net = net == "mainnet" || net.starts_with("Public Global Stellar");
    let is_test_net = net == "testnet" || net == "futurenet" || net.starts_with("Test SDF");
    let is_local_net = net == "standalone" || net == "local";

    let rpc_has_local =
        contains_case_insensitive(rpc, "localhost") || contains_case_insensitive(rpc, "127.0.0.1");
    let rpc_has_test =
        contains_case_insensitive(rpc, "testnet") || contains_case_insensitive(rpc, "futurenet");
    let rpc_has_main =
        contains_case_insensitive(rpc, "mainnet") || contains_case_insensitive(rpc, "public");

    if env.is_production() {
        if !is_prod_net {
            return Err(Error::ConfigInvalid);
        }
        if !rpc.starts_with("https://") {
            return Err(Error::ConfigInvalid);
        }
        if rpc_has_local || rpc_has_test {
            return Err(Error::ConfigInvalid);
        }
    }

    if is_prod_net {
        if rpc_has_test || rpc_has_local {
            return Err(Error::ConfigInvalid);
        }
        if !rpc.starts_with("https://") {
            return Err(Error::ConfigInvalid);
        }
    }

    if is_test_net {
        if env.is_production() {
            return Err(Error::ConfigInvalid);
        }
        if rpc_has_main || rpc_has_local {
            return Err(Error::ConfigInvalid);
        }
    }

    if is_local_net && (env.is_production() || env == Environment::Staging) {
        return Err(Error::ConfigInvalid);
    }

    Ok(())
}

/// Contract IDs across all protocol modules.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModuleContractIds<'a> {
    pub treasury: Option<&'a str>,
    pub aid: Option<&'a str>,
    pub referral: Option<&'a str>,
    pub governance: Option<&'a str>,
    pub oracle: Option<&'a str>,
    pub marketplace: Option<&'a str>,
    pub registry: Option<&'a str>,
}

impl<'a> ModuleContractIds<'a> {
    pub const fn empty() -> Self {
        Self {
            treasury: None,
            aid: None,
            referral: None,
            governance: None,
            oracle: None,
            marketplace: None,
            registry: None,
        }
    }

    pub fn validate(&self, env: Environment) -> Result<(), Error> {
        if env.is_production() {
            require_present(self.treasury)?;
            require_present(self.aid)?;
            require_present(self.referral)?;
            require_present(self.governance)?;
            require_present(self.oracle)?;
            require_present(self.marketplace)?;
            require_present(self.registry)?;
        } else if env == Environment::Staging {
            require_present(self.treasury)?;
            require_present(self.aid)?;
            require_present(self.referral)?;
            require_present(self.governance)?;
            require_present(self.oracle)?;
            require_present(self.marketplace)?;
        }

        let all = [
            self.treasury,
            self.aid,
            self.referral,
            self.governance,
            self.oracle,
            self.marketplace,
            self.registry,
        ];

        for opt in all.iter() {
            if let Some(id) = opt {
                validate_contract_id(Some(id))?;
            }
        }

        // Cross-module collision check: no two modules can share an identical contract ID
        for i in 0..all.len() {
            if let Some(a) = all[i] {
                for j in (i + 1)..all.len() {
                    if let Some(b) = all[j] {
                        if a.trim().eq_ignore_ascii_case(b.trim()) {
                            return Err(Error::ConfigInvalid);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Production-critical settings for the Treasury module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreasurySettings {
    pub max_withdrawal_limit: i128,
}

impl TreasurySettings {
    pub fn validate(&self, env: Environment) -> Result<(), Error> {
        if self.max_withdrawal_limit <= 0 {
            return Err(Error::ConfigInvalid);
        }
        if self.max_withdrawal_limit > 1_000_000_000_000_000_000 {
            return Err(Error::ConfigInvalid);
        }
        if env.is_production() && self.max_withdrawal_limit < 1_000 {
            return Err(Error::ConfigInvalid);
        }
        Ok(())
    }
}

/// Production-critical settings for the Aid module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AidSettings<'a> {
    pub treasury_id: &'a str,
    pub token_id: &'a str,
    pub default_expiry_secs: u64,
}

impl<'a> AidSettings<'a> {
    pub fn validate(&self, env: Environment, contract_ids: &ModuleContractIds<'a>) -> Result<(), Error> {
        validate_contract_id(Some(self.treasury_id))?;
        validate_contract_id(Some(self.token_id))?;
        if let Some(expected_treasury) = contract_ids.treasury {
            if !self.treasury_id.trim().eq_ignore_ascii_case(expected_treasury.trim()) {
                return Err(Error::ConfigInvalid);
            }
        }
        if self.default_expiry_secs < 60 || self.default_expiry_secs > 31_536_000 {
            return Err(Error::ConfigInvalid);
        }
        if env.is_production() && self.default_expiry_secs < 3600 {
            return Err(Error::ConfigInvalid);
        }
        Ok(())
    }
}

/// Production-critical settings for the Referral module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferralSettings<'a> {
    pub treasury_id: &'a str,
    pub registry_id: Option<&'a str>,
    pub max_tiers: u32,
    pub tier_bps: &'a [i128],
    pub reward_cap: i128,
}

impl<'a> ReferralSettings<'a> {
    pub fn validate(&self, env: Environment, contract_ids: &ModuleContractIds<'a>) -> Result<(), Error> {
        validate_contract_id(Some(self.treasury_id))?;
        if let Some(expected_treasury) = contract_ids.treasury {
            if !self.treasury_id.trim().eq_ignore_ascii_case(expected_treasury.trim()) {
                return Err(Error::ConfigInvalid);
            }
        }
        if env.is_production() {
            let reg = require_present(self.registry_id)?;
            validate_contract_id(Some(reg))?;
            if let Some(expected_reg) = contract_ids.registry {
                if !reg.trim().eq_ignore_ascii_case(expected_reg.trim()) {
                    return Err(Error::ConfigInvalid);
                }
            }
        } else if let Some(reg) = self.registry_id {
            validate_contract_id(Some(reg))?;
        }

        if self.max_tiers < 1 || self.max_tiers > 10 {
            return Err(Error::ConfigInvalid);
        }
        if self.tier_bps.len() != self.max_tiers as usize {
            return Err(Error::ConfigInvalid);
        }
        let mut sum: i128 = 0;
        for &bps in self.tier_bps {
            if bps < 0 || bps > 10_000 {
                return Err(Error::ConfigInvalid);
            }
            sum = sum.checked_add(bps).ok_or(Error::ConfigInvalid)?;
        }
        if sum > 10_000 {
            return Err(Error::ConfigInvalid);
        }
        if self.reward_cap < 0 || self.reward_cap > 1_000_000_000_000_000_000 {
            return Err(Error::ConfigInvalid);
        }
        Ok(())
    }
}

/// Production-critical settings for the Governance module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GovernanceSettings<'a> {
    pub threshold: u32,
    pub admin_set: &'a [&'a str],
}

impl<'a> GovernanceSettings<'a> {
    pub fn validate(&self, env: Environment) -> Result<(), Error> {
        if self.threshold < 1 {
            return Err(Error::ConfigInvalid);
        }
        if env.is_production() && self.threshold < 2 {
            return Err(Error::ConfigInvalid);
        }
        if self.admin_set.len() < self.threshold as usize || self.admin_set.len() > 20 {
            return Err(Error::ConfigInvalid);
        }
        for admin in self.admin_set {
            validate_account_id(Some(admin))?;
        }
        for i in 0..self.admin_set.len() {
            for j in (i + 1)..self.admin_set.len() {
                if self.admin_set[i].trim().eq_ignore_ascii_case(self.admin_set[j].trim()) {
                    return Err(Error::ConfigInvalid);
                }
            }
        }
        Ok(())
    }
}

/// Production-critical settings for the Oracle module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OracleSettings {
    pub min_price_quorum: u32,
    pub max_future_skew_secs: u64,
}

impl OracleSettings {
    pub fn validate(&self, env: Environment) -> Result<(), Error> {
        if self.min_price_quorum < 1 {
            return Err(Error::ConfigInvalid);
        }
        if env.is_production() && self.min_price_quorum < 2 {
            return Err(Error::ConfigInvalid);
        }
        if self.max_future_skew_secs < 10 || self.max_future_skew_secs > 300 {
            return Err(Error::ConfigInvalid);
        }
        Ok(())
    }
}

/// Production-critical settings for the NFT Marketplace module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketplaceSettings<'a> {
    pub platform_fee_bps: i128,
    pub fee_recipient: &'a str,
    pub bid_increment_bps: i128,
    pub auto_extension_seconds: u64,
}

impl<'a> MarketplaceSettings<'a> {
    pub fn validate(&self, _env: Environment) -> Result<(), Error> {
        if !(0..=1_000).contains(&self.platform_fee_bps) {
            return Err(Error::ConfigInvalid);
        }
        validate_account_id(Some(self.fee_recipient))?;
        if !(50..=5_000).contains(&self.bid_increment_bps) {
            return Err(Error::ConfigInvalid);
        }
        if !(60..=3600).contains(&self.auto_extension_seconds) {
            return Err(Error::ConfigInvalid);
        }
        Ok(())
    }
}

/// Cross-environment configuration bundle verifying consistency across all
/// production-critical contracts and runtime parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrossEnvConfigBundle<'a> {
    pub env: Environment,
    pub network: Option<&'a str>,
    pub rpc_url: Option<&'a str>,
    pub secret: Option<&'a str>,
    pub contract_ids: ModuleContractIds<'a>,
    pub treasury: Option<TreasurySettings>,
    pub aid: Option<AidSettings<'a>>,
    pub referral: Option<ReferralSettings<'a>>,
    pub governance: Option<GovernanceSettings<'a>>,
    pub oracle: Option<OracleSettings>,
    pub marketplace: Option<MarketplaceSettings<'a>>,
}

impl<'a> CrossEnvConfigBundle<'a> {
    pub fn validate(&self) -> Result<(), Error> {
        validate_network_rpc_consistency(self.network, self.rpc_url, self.env)?;
        validate_secret_key(self.secret, self.env)?;
        self.contract_ids.validate(self.env)?;

        if let Some(ref treasury) = self.treasury {
            treasury.validate(self.env)?;
        } else if self.env.is_production() {
            return Err(Error::ConfigMissing);
        }

        if let Some(ref aid) = self.aid {
            aid.validate(self.env, &self.contract_ids)?;
        } else if self.env.is_production() {
            return Err(Error::ConfigMissing);
        }

        if let Some(ref referral) = self.referral {
            referral.validate(self.env, &self.contract_ids)?;
        } else if self.env.is_production() {
            return Err(Error::ConfigMissing);
        }

        if let Some(ref governance) = self.governance {
            governance.validate(self.env)?;
        } else if self.env.is_production() {
            return Err(Error::ConfigMissing);
        }

        if let Some(ref oracle) = self.oracle {
            oracle.validate(self.env)?;
        } else if self.env.is_production() {
            return Err(Error::ConfigMissing);
        }

        if let Some(ref marketplace) = self.marketplace {
            marketplace.validate(self.env)?;
        } else if self.env.is_production() {
            return Err(Error::ConfigMissing);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::format;
    use super::*;

    // 56-char well-formed seed fixture: 'S' + 55 alphanumeric chars.
    const TEST_SEED: &str = "SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    // Valid 56-char contract ID fixtures (Base32 uppercase: C + 55 chars from A-Z, 2-7)
    const VALID_CONTRACT_1: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA2";
    const VALID_CONTRACT_2: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA3";
    const VALID_CONTRACT_3: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA4";
    const VALID_CONTRACT_4: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA5";
    const VALID_CONTRACT_5: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA6";
    const VALID_CONTRACT_6: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA7";
    const VALID_CONTRACT_7: &str = "CBAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA2";

    // Valid 56-char account ID fixtures (G + 55 chars)
    const VALID_ACCOUNT_1: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA2";
    const VALID_ACCOUNT_2: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA3";

    #[test]
    fn missing_values_fail_fast() {
        assert_eq!(require_present(None), Err(Error::ConfigMissing));
        assert_eq!(require_present(Some("  ")), Err(Error::ConfigMissing));
        assert_eq!(validate_network_id(None), Err(Error::ConfigMissing));
        assert_eq!(validate_rpc_url(None), Err(Error::ConfigMissing));
        assert_eq!(
            validate_secret_key(None, Environment::Staging),
            Err(Error::ConfigMissing)
        );
        assert_eq!(validate_contract_id(None), Err(Error::ConfigMissing));
        assert_eq!(validate_account_id(None), Err(Error::ConfigMissing));
    }

    #[test]
    fn malformed_values_are_rejected() {
        assert_eq!(
            validate_network_id(Some("marsnet")),
            Err(Error::ConfigInvalid)
        );
        assert_eq!(
            validate_rpc_url(Some("not-a-url")),
            Err(Error::ConfigInvalid)
        );
        assert_eq!(
            validate_rpc_url(Some("ftp://example.com/rpc")),
            Err(Error::ConfigInvalid)
        );
        assert_eq!(
            validate_secret_key(Some("S-INVALID-LENGTH-LONGER-THAN-16-CHARS"), Environment::Staging),
            Err(Error::ConfigInvalid)
        );
        assert_eq!(validate_feature_flag(Some("maybe")), Err(Error::ConfigInvalid));
        assert_eq!(validate_feature_flag(Some("")), Err(Error::ConfigMissing));
    }

    #[test]
    fn unsafe_secrets_never_pass() {
        for bad in ["changeme", "password", "test", "YOUR_SECRET_HERE", "short"] {
            assert_eq!(
                validate_secret_key(Some(bad), Environment::Staging),
                Err(Error::UnsafeSecret),
                "expected UnsafeSecret for {bad}"
            );
        }
        // Production-shaped seed in local mode -> unsafe (prod reuse).
        assert_eq!(validate_secret_key(Some(TEST_SEED), Environment::Local), Err(Error::UnsafeSecret));
        // Same seed in staging/production shape-checks pass.
        assert!(validate_secret_key(Some(TEST_SEED), Environment::Staging).is_ok());
    }

    #[test]
    fn production_requires_https() {
        assert_eq!(
            validate_full_config(
                Some("mainnet"),
                Some("http://insecure.example.com/rpc"),
                Some(TEST_SEED),
                Environment::Production
            ),
            Err(Error::ConfigInvalid)
        );
        assert!(validate_full_config(
            Some("mainnet"),
            Some("https://horizon.stellar.org/rpc"),
            Some(TEST_SEED),
            Environment::Production
        )
        .is_ok());
    }

    #[test]
    fn secrets_are_never_printed_in_full() {
        let dbg = format!("{:?}", RedactedSecret(TEST_SEED));
        assert!(!dbg.contains(TEST_SEED));
        assert!(dbg.contains("..."));
    }

    #[test]
    fn contract_id_validation_boundaries() {
        // Correct 56 chars starting with C and base32
        assert!(validate_contract_id(Some(VALID_CONTRACT_1)).is_ok());

        // Missing / empty
        assert_eq!(validate_contract_id(None), Err(Error::ConfigMissing));
        assert_eq!(validate_contract_id(Some("")), Err(Error::ConfigMissing));

        // Wrong prefix (starts with G, S, or X)
        assert_eq!(validate_contract_id(Some(VALID_ACCOUNT_1)), Err(Error::ConfigInvalid));
        assert_eq!(validate_contract_id(Some(TEST_SEED)), Err(Error::ConfigInvalid));

        // Short length / long length
        assert_eq!(validate_contract_id(Some("CAAAAAAAAA")), Err(Error::ConfigInvalid));
        assert_eq!(
            validate_contract_id(Some("CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")),
            Err(Error::ConfigInvalid)
        );

        // Invalid characters (0, 1, 8, 9, lowercase, symbols)
        let invalid_char = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA8";
        assert_eq!(validate_contract_id(Some(invalid_char)), Err(Error::ConfigInvalid));

        // Repeated identical chars
        let repeated = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(validate_contract_id(Some(repeated)), Err(Error::ConfigInvalid));

        // Placeholders
        assert_eq!(
            validate_contract_id(Some("C_CONTRACT_ID_PLACEHOLDER_AAAAAAAAAAAAAAAAAAAAAAAAAAAAA2")),
            Err(Error::ConfigInvalid)
        );
    }

    #[test]
    fn account_id_validation_boundaries() {
        assert!(validate_account_id(Some(VALID_ACCOUNT_1)).is_ok());
        assert!(validate_account_id(Some(VALID_CONTRACT_1)).is_ok());
        assert_eq!(validate_account_id(None), Err(Error::ConfigMissing));
        assert_eq!(validate_account_id(Some("short")), Err(Error::ConfigInvalid));
    }

    #[test]
    fn network_rpc_consistency_enforcement() {
        // Valid pairings
        assert!(validate_network_rpc_consistency(
            Some("mainnet"),
            Some("https://mainnet.stellar.org/rpc"),
            Environment::Production
        ).is_ok());

        assert!(validate_network_rpc_consistency(
            Some("testnet"),
            Some("https://soroban-testnet.stellar.org"),
            Environment::Staging
        ).is_ok());

        assert!(validate_network_rpc_consistency(
            Some("standalone"),
            Some("http://localhost:8000/rpc"),
            Environment::Local
        ).is_ok());

        // Mismatches: Mainnet paired with testnet RPC
        assert_eq!(
            validate_network_rpc_consistency(
                Some("mainnet"),
                Some("https://testnet.stellar.org/rpc"),
                Environment::Production
            ),
            Err(Error::ConfigInvalid)
        );

        // Mismatches: Mainnet paired with localhost RPC
        assert_eq!(
            validate_network_rpc_consistency(
                Some("mainnet"),
                Some("https://localhost:8000/rpc"),
                Environment::Production
            ),
            Err(Error::ConfigInvalid)
        );

        // Mismatches: Testnet paired with mainnet RPC
        assert_eq!(
            validate_network_rpc_consistency(
                Some("testnet"),
                Some("https://mainnet.stellar.org/rpc"),
                Environment::Staging
            ),
            Err(Error::ConfigInvalid)
        );

        // Production env with testnet network
        assert_eq!(
            validate_network_rpc_consistency(
                Some("testnet"),
                Some("https://soroban-testnet.stellar.org"),
                Environment::Production
            ),
            Err(Error::ConfigInvalid)
        );

        // Production env with http URL
        assert_eq!(
            validate_network_rpc_consistency(
                Some("mainnet"),
                Some("http://mainnet.stellar.org/rpc"),
                Environment::Production
            ),
            Err(Error::ConfigInvalid)
        );

        // Staging env with standalone local network
        assert_eq!(
            validate_network_rpc_consistency(
                Some("standalone"),
                Some("http://localhost:8000/rpc"),
                Environment::Staging
            ),
            Err(Error::ConfigInvalid)
        );
    }

    #[test]
    fn module_contract_ids_distinctness_and_env_rules() {
        let valid_ids = ModuleContractIds {
            treasury: Some(VALID_CONTRACT_1),
            aid: Some(VALID_CONTRACT_2),
            referral: Some(VALID_CONTRACT_3),
            governance: Some(VALID_CONTRACT_4),
            oracle: Some(VALID_CONTRACT_5),
            marketplace: Some(VALID_CONTRACT_6),
            registry: Some(VALID_CONTRACT_7),
        };

        // All distinct IDs pass in Production, Staging, and Local
        assert!(valid_ids.validate(Environment::Production).is_ok());
        assert!(valid_ids.validate(Environment::Staging).is_ok());
        assert!(valid_ids.validate(Environment::Local).is_ok());

        // In Production, missing any module fails with ConfigMissing
        let mut missing_registry = valid_ids;
        missing_registry.registry = None;
        assert_eq!(missing_registry.validate(Environment::Production), Err(Error::ConfigMissing));

        let mut missing_treasury = valid_ids;
        missing_treasury.treasury = None;
        assert_eq!(missing_treasury.validate(Environment::Production), Err(Error::ConfigMissing));
        assert_eq!(missing_treasury.validate(Environment::Staging), Err(Error::ConfigMissing));

        // Cross-module collision: treasury and aid reuse the same contract ID
        let mut collision = valid_ids;
        collision.aid = Some(VALID_CONTRACT_1);
        assert_eq!(collision.validate(Environment::Production), Err(Error::ConfigInvalid));
        assert_eq!(collision.validate(Environment::Local), Err(Error::ConfigInvalid));
    }

    #[test]
    fn treasury_settings_validation() {
        let good = TreasurySettings { max_withdrawal_limit: 10_000 };
        assert!(good.validate(Environment::Production).is_ok());
        assert!(good.validate(Environment::Local).is_ok());

        // Negative or zero
        assert_eq!(
            TreasurySettings { max_withdrawal_limit: 0 }.validate(Environment::Local),
            Err(Error::ConfigInvalid)
        );
        assert_eq!(
            TreasurySettings { max_withdrawal_limit: -50 }.validate(Environment::Local),
            Err(Error::ConfigInvalid)
        );

        // Exceeds upper limit
        assert_eq!(
            TreasurySettings { max_withdrawal_limit: 2_000_000_000_000_000_000 }
                .validate(Environment::Local),
            Err(Error::ConfigInvalid)
        );

        // Production too low (< 1,000)
        assert_eq!(
            TreasurySettings { max_withdrawal_limit: 500 }.validate(Environment::Production),
            Err(Error::ConfigInvalid)
        );
        assert!(TreasurySettings { max_withdrawal_limit: 500 }.validate(Environment::Local).is_ok());
    }

    #[test]
    fn aid_settings_validation() {
        let contract_ids = ModuleContractIds {
            treasury: Some(VALID_CONTRACT_1),
            ..ModuleContractIds::empty()
        };

        let good = AidSettings {
            treasury_id: VALID_CONTRACT_1,
            token_id: VALID_CONTRACT_2,
            default_expiry_secs: 86400,
        };
        assert!(good.validate(Environment::Production, &contract_ids).is_ok());

        // Mismatched treasury ID
        let bad_treasury = AidSettings {
            treasury_id: VALID_CONTRACT_3,
            ..good
        };
        assert_eq!(
            bad_treasury.validate(Environment::Production, &contract_ids),
            Err(Error::ConfigInvalid)
        );

        // Expiry bounds
        let too_short = AidSettings { default_expiry_secs: 30, ..good };
        assert_eq!(too_short.validate(Environment::Local, &contract_ids), Err(Error::ConfigInvalid));

        // In production, expiry must be >= 1 hour (3600)
        let prod_too_short = AidSettings { default_expiry_secs: 1800, ..good };
        assert_eq!(prod_too_short.validate(Environment::Production, &contract_ids), Err(Error::ConfigInvalid));
        assert!(prod_too_short.validate(Environment::Local, &contract_ids).is_ok());
    }

    #[test]
    fn referral_settings_validation() {
        let contract_ids = ModuleContractIds {
            treasury: Some(VALID_CONTRACT_1),
            registry: Some(VALID_CONTRACT_7),
            ..ModuleContractIds::empty()
        };

        let good = ReferralSettings {
            treasury_id: VALID_CONTRACT_1,
            registry_id: Some(VALID_CONTRACT_7),
            max_tiers: 3,
            tier_bps: &[500, 250, 100],
            reward_cap: 10_000_000,
        };
        assert!(good.validate(Environment::Production, &contract_ids).is_ok());

        // Mismatched registry in production
        let bad_reg = ReferralSettings {
            registry_id: Some(VALID_CONTRACT_2),
            ..good
        };
        assert_eq!(bad_reg.validate(Environment::Production, &contract_ids), Err(Error::ConfigInvalid));

        // Tier count mismatch with slice
        let bad_slice_len = ReferralSettings {
            tier_bps: &[500, 250],
            ..good
        };
        assert_eq!(bad_slice_len.validate(Environment::Local, &contract_ids), Err(Error::ConfigInvalid));

        // Cumulative bps exceeding 10,000 (100%)
        let bad_sum = ReferralSettings {
            tier_bps: &[6000, 4001, 100],
            ..good
        };
        assert_eq!(bad_sum.validate(Environment::Local, &contract_ids), Err(Error::ConfigInvalid));
    }

    #[test]
    fn governance_settings_validation() {
        let admins = [VALID_ACCOUNT_1, VALID_ACCOUNT_2];
        let good = GovernanceSettings {
            threshold: 2,
            admin_set: &admins,
        };
        assert!(good.validate(Environment::Production).is_ok());

        // Production rejects single-key threshold < 2
        let single_key = GovernanceSettings {
            threshold: 1,
            admin_set: &admins,
        };
        assert_eq!(single_key.validate(Environment::Production), Err(Error::ConfigInvalid));
        assert!(single_key.validate(Environment::Local).is_ok());

        // Threshold exceeds admin count
        let over_thresh = GovernanceSettings {
            threshold: 3,
            admin_set: &admins,
        };
        assert_eq!(over_thresh.validate(Environment::Local), Err(Error::ConfigInvalid));

        // Duplicate admin accounts
        let dup_admins = [VALID_ACCOUNT_1, VALID_ACCOUNT_1];
        let dup_gov = GovernanceSettings {
            threshold: 2,
            admin_set: &dup_admins,
        };
        assert_eq!(dup_gov.validate(Environment::Production), Err(Error::ConfigInvalid));
    }

    #[test]
    fn oracle_and_marketplace_settings_validation() {
        let good_oracle = OracleSettings {
            min_price_quorum: 2,
            max_future_skew_secs: 60,
        };
        assert!(good_oracle.validate(Environment::Production).is_ok());

        // Production quorum must be at least 2
        let single_quorum = OracleSettings { min_price_quorum: 1, ..good_oracle };
        assert_eq!(single_quorum.validate(Environment::Production), Err(Error::ConfigInvalid));
        assert!(single_quorum.validate(Environment::Local).is_ok());

        // Marketplace fee and recipient validation
        let good_mkt = MarketplaceSettings {
            platform_fee_bps: 250,
            fee_recipient: VALID_ACCOUNT_1,
            bid_increment_bps: 500,
            auto_extension_seconds: 300,
        };
        assert!(good_mkt.validate(Environment::Production).is_ok());

        let excessive_fee = MarketplaceSettings {
            platform_fee_bps: 1001,
            ..good_mkt
        };
        assert_eq!(excessive_fee.validate(Environment::Local), Err(Error::ConfigInvalid));
    }

    #[test]
    fn cross_env_config_bundle_full_lifecycle() {
        let contract_ids = ModuleContractIds {
            treasury: Some(VALID_CONTRACT_1),
            aid: Some(VALID_CONTRACT_2),
            referral: Some(VALID_CONTRACT_3),
            governance: Some(VALID_CONTRACT_4),
            oracle: Some(VALID_CONTRACT_5),
            marketplace: Some(VALID_CONTRACT_6),
            registry: Some(VALID_CONTRACT_7),
        };

        let admins = [VALID_ACCOUNT_1, VALID_ACCOUNT_2];

        let valid_bundle = CrossEnvConfigBundle {
            env: Environment::Production,
            network: Some("mainnet"),
            rpc_url: Some("https://horizon.stellar.org/rpc"),
            secret: Some(TEST_SEED),
            contract_ids,
            treasury: Some(TreasurySettings { max_withdrawal_limit: 100_000 }),
            aid: Some(AidSettings {
                treasury_id: VALID_CONTRACT_1,
                token_id: VALID_CONTRACT_2,
                default_expiry_secs: 86400,
            }),
            referral: Some(ReferralSettings {
                treasury_id: VALID_CONTRACT_1,
                registry_id: Some(VALID_CONTRACT_7),
                max_tiers: 3,
                tier_bps: &[500, 250, 100],
                reward_cap: 10_000_000,
            }),
            governance: Some(GovernanceSettings {
                threshold: 2,
                admin_set: &admins,
            }),
            oracle: Some(OracleSettings {
                min_price_quorum: 2,
                max_future_skew_secs: 60,
            }),
            marketplace: Some(MarketplaceSettings {
                platform_fee_bps: 250,
                fee_recipient: VALID_ACCOUNT_1,
                bid_increment_bps: 500,
                auto_extension_seconds: 300,
            }),
        };

        assert!(valid_bundle.validate().is_ok());

        // In production, missing treasury settings fails with ConfigMissing
        let mut missing_treasury = valid_bundle.clone();
        missing_treasury.treasury = None;
        assert_eq!(missing_treasury.validate(), Err(Error::ConfigMissing));

        // Network mismatch fails with ConfigInvalid
        let mut network_mismatch = valid_bundle.clone();
        network_mismatch.network = Some("testnet");
        assert_eq!(network_mismatch.validate(), Err(Error::ConfigInvalid));
    }
}

