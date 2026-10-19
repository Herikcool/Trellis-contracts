#![cfg(test)]

//! Automated cross-environment configuration contract tests for production-critical settings.
//!
//! Verifies configuration consistency, missing/malformed environment variables, contract IDs,
//! network settings, URLs, secrets, and module parameters across Local, Staging, and Production.

extern crate std;

use shared::config::{
    validate_network_rpc_consistency, validate_secret_key, AidSettings, CrossEnvConfigBundle,
    Environment, GovernanceSettings, MarketplaceSettings, ModuleContractIds, OracleSettings,
    ReferralSettings, TreasurySettings,
};
use shared::errors::Error;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{token, Address, Env};

// Realistic 56-char base32 contract IDs for tests
const TREASURY_ID: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA2";
const AID_ID: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA3";
const REFERRAL_ID: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA4";
const GOVERNANCE_ID: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA5";
const ORACLE_ID: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA6";
const MARKETPLACE_ID: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA7";
const REGISTRY_ID: &str = "CBAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA2";

// 56-character valid seed: 'S' + 55 base32 chars
const PROD_SEED: &str = "SBCDEFGHIJKLMNOPQRSTUVWXYZ234567BCDEFGHIJKLMNOPQRSTUVWXY";
const ADMIN_1: &str = "GA2AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA2";
const ADMIN_2: &str = "GA3AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA3";

fn setup_test_token<'a>(
    env: &'a Env,
    admin: &Address,
) -> (Address, token::Client<'a>, token::StellarAssetClient<'a>) {
    let contract_address = env.register_stellar_asset_contract(admin.clone());
    let client = token::Client::new(env, &contract_address);
    let asset_client = token::StellarAssetClient::new(env, &contract_address);
    (contract_address, client, asset_client)
}

#[test]
fn test_cross_env_local_configuration() {
    let contract_ids = ModuleContractIds {
        treasury: Some(TREASURY_ID),
        aid: Some(AID_ID),
        ..ModuleContractIds::empty()
    };

    let local_bundle = CrossEnvConfigBundle {
        env: Environment::Local,
        network: Some("standalone"),
        rpc_url: Some("http://localhost:8000"),
        secret: None,
        contract_ids,
        treasury: Some(TreasurySettings {
            max_withdrawal_limit: 500, // relaxed in local
        }),
        aid: Some(AidSettings {
            treasury_id: TREASURY_ID,
            token_id: AID_ID,
            default_expiry_secs: 120, // relaxed in local
        }),
        referral: None,
        governance: None,
        oracle: None,
        marketplace: None,
    };

    assert!(local_bundle.validate().is_ok());
}

#[test]
fn test_cross_env_staging_configuration() {
    let contract_ids = ModuleContractIds {
        treasury: Some(TREASURY_ID),
        aid: Some(AID_ID),
        referral: Some(REFERRAL_ID),
        governance: Some(GOVERNANCE_ID),
        oracle: Some(ORACLE_ID),
        marketplace: Some(MARKETPLACE_ID),
        registry: None,
    };

    let admins = [ADMIN_1];

    let staging_bundle = CrossEnvConfigBundle {
        env: Environment::Staging,
        network: Some("testnet"),
        rpc_url: Some("https://soroban-testnet.stellar.org"),
        secret: Some(PROD_SEED),
        contract_ids,
        treasury: Some(TreasurySettings {
            max_withdrawal_limit: 10_000_000,
        }),
        aid: Some(AidSettings {
            treasury_id: TREASURY_ID,
            token_id: AID_ID,
            default_expiry_secs: 86400,
        }),
        referral: Some(ReferralSettings {
            treasury_id: TREASURY_ID,
            registry_id: None,
            max_tiers: 2,
            tier_bps: &[300, 100],
            reward_cap: 5_000_000,
        }),
        governance: Some(GovernanceSettings {
            threshold: 1, // 1 of 1 allowed in staging
            admin_set: &admins,
        }),
        oracle: Some(OracleSettings {
            min_price_quorum: 1, // 1 allowed in staging
            max_future_skew_secs: 60,
        }),
        marketplace: Some(MarketplaceSettings {
            platform_fee_bps: 200,
            fee_recipient: ADMIN_1,
            bid_increment_bps: 500,
            auto_extension_seconds: 300,
        }),
    };

    assert!(staging_bundle.validate().is_ok());
}

#[test]
fn test_cross_env_production_configuration() {
    let contract_ids = ModuleContractIds {
        treasury: Some(TREASURY_ID),
        aid: Some(AID_ID),
        referral: Some(REFERRAL_ID),
        governance: Some(GOVERNANCE_ID),
        oracle: Some(ORACLE_ID),
        marketplace: Some(MARKETPLACE_ID),
        registry: Some(REGISTRY_ID),
    };

    let admins = [ADMIN_1, ADMIN_2];

    let prod_bundle = CrossEnvConfigBundle {
        env: Environment::Production,
        network: Some("mainnet"),
        rpc_url: Some("https://horizon.stellar.org/rpc"),
        secret: Some(PROD_SEED),
        contract_ids,
        treasury: Some(TreasurySettings {
            max_withdrawal_limit: 50_000_000,
        }),
        aid: Some(AidSettings {
            treasury_id: TREASURY_ID,
            token_id: AID_ID,
            default_expiry_secs: 604_800, // 7 days (>= 3600)
        }),
        referral: Some(ReferralSettings {
            treasury_id: TREASURY_ID,
            registry_id: Some(REGISTRY_ID),
            max_tiers: 3,
            tier_bps: &[500, 250, 100],
            reward_cap: 10_000_000,
        }),
        governance: Some(GovernanceSettings {
            threshold: 2, // multi-sig required in prod
            admin_set: &admins,
        }),
        oracle: Some(OracleSettings {
            min_price_quorum: 2, // quorum >= 2 in prod
            max_future_skew_secs: 60,
        }),
        marketplace: Some(MarketplaceSettings {
            platform_fee_bps: 250,
            fee_recipient: ADMIN_1,
            bid_increment_bps: 500,
            auto_extension_seconds: 300,
        }),
    };

    assert!(prod_bundle.validate().is_ok());
}

#[test]
fn test_production_fails_when_any_module_contract_id_missing() {
    let mut contract_ids = ModuleContractIds {
        treasury: Some(TREASURY_ID),
        aid: Some(AID_ID),
        referral: Some(REFERRAL_ID),
        governance: Some(GOVERNANCE_ID),
        oracle: Some(ORACLE_ID),
        marketplace: Some(MARKETPLACE_ID),
        registry: Some(REGISTRY_ID),
    };

    // Remove marketplace
    contract_ids.marketplace = None;
    assert_eq!(contract_ids.validate(Environment::Production), Err(Error::ConfigMissing));

    // Remove registry
    contract_ids.marketplace = Some(MARKETPLACE_ID);
    contract_ids.registry = None;
    assert_eq!(contract_ids.validate(Environment::Production), Err(Error::ConfigMissing));
}

#[test]
fn test_production_fails_when_network_or_rpc_inconsistent() {
    // Mainnet with testnet RPC
    assert_eq!(
        validate_network_rpc_consistency(
            Some("mainnet"),
            Some("https://soroban-testnet.stellar.org"),
            Environment::Production
        ),
        Err(Error::ConfigInvalid)
    );

    // Mainnet with localhost RPC
    assert_eq!(
        validate_network_rpc_consistency(
            Some("mainnet"),
            Some("http://127.0.0.1:8000"),
            Environment::Production
        ),
        Err(Error::ConfigInvalid)
    );

    // Testnet network in production environment
    assert_eq!(
        validate_network_rpc_consistency(
            Some("testnet"),
            Some("https://soroban-testnet.stellar.org"),
            Environment::Production
        ),
        Err(Error::ConfigInvalid)
    );

    // Insecure HTTP in production
    assert_eq!(
        validate_network_rpc_consistency(
            Some("mainnet"),
            Some("http://horizon.stellar.org"),
            Environment::Production
        ),
        Err(Error::ConfigInvalid)
    );
}

#[test]
fn test_production_fails_when_secret_is_unsafe_or_missing() {
    // Missing secret
    assert_eq!(validate_secret_key(None, Environment::Production), Err(Error::ConfigMissing));

    // Placeholder secret
    assert_eq!(
        validate_secret_key(Some("YOUR_SECRET_HERE"), Environment::Production),
        Err(Error::UnsafeSecret)
    );
    assert_eq!(
        validate_secret_key(Some("changeme"), Environment::Production),
        Err(Error::UnsafeSecret)
    );
}

#[test]
fn test_cross_module_contract_id_collision_fails() {
    // Two modules pointing to the same contract ID
    let collision_ids = ModuleContractIds {
        treasury: Some(TREASURY_ID),
        aid: Some(TREASURY_ID), // collision!
        referral: Some(REFERRAL_ID),
        governance: Some(GOVERNANCE_ID),
        oracle: Some(ORACLE_ID),
        marketplace: Some(MARKETPLACE_ID),
        registry: Some(REGISTRY_ID),
    };

    assert_eq!(collision_ids.validate(Environment::Production), Err(Error::ConfigInvalid));
}

#[test]
fn test_cross_contract_reference_mismatch_fails() {
    let contract_ids = ModuleContractIds {
        treasury: Some(TREASURY_ID),
        aid: Some(AID_ID),
        referral: Some(REFERRAL_ID),
        governance: Some(GOVERNANCE_ID),
        oracle: Some(ORACLE_ID),
        marketplace: Some(MARKETPLACE_ID),
        registry: Some(REGISTRY_ID),
    };

    // Aid points to wrong treasury ID
    let bad_aid = AidSettings {
        treasury_id: REFERRAL_ID, // mismatch!
        token_id: AID_ID,
        default_expiry_secs: 86400,
    };
    assert_eq!(
        bad_aid.validate(Environment::Production, &contract_ids),
        Err(Error::ConfigInvalid)
    );

    // Referral points to wrong treasury ID
    let bad_ref = ReferralSettings {
        treasury_id: AID_ID, // mismatch!
        registry_id: Some(REGISTRY_ID),
        max_tiers: 1,
        tier_bps: &[100],
        reward_cap: 1_000_000,
    };
    assert_eq!(
        bad_ref.validate(Environment::Production, &contract_ids),
        Err(Error::ConfigInvalid)
    );
}

#[test]
fn test_onchain_env_wiring_and_balances() {
    // Verify that Soroban test environment operates cleanly with contract addresses
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_token_addr, token_client, asset_client) = setup_test_token(&env, &admin);

    let treasury_addr = Address::generate(&env);
    let aid_escrow = Address::generate(&env);
    let donor = Address::generate(&env);

    asset_client.mint(&donor, &50_000);
    assert_eq!(token_client.balance(&donor), 50_000);

    // Simulate aid escrow deposit
    token_client.transfer(&donor, &aid_escrow, &20_000);
    assert_eq!(token_client.balance(&aid_escrow), 20_000);
    assert_eq!(token_client.balance(&donor), 30_000);

    // Simulate fee payment to treasury
    token_client.transfer(&donor, &treasury_addr, &1_000);
    assert_eq!(token_client.balance(&treasury_addr), 1_000);
    assert_eq!(token_client.balance(&donor), 29_000);
}
