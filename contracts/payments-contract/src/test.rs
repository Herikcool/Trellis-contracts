#![cfg(test)]

extern crate std;

use super::*;
use shared::payments::EscrowState;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Env,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

struct Fixture {
    env: Env,
    admin: Address,
    token_addr: Address,
    token_client: token::Client<'static>,
    contract_id: Address,
    client: ExamplePaymentsContractClient<'static>,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_addr = env.register_stellar_asset_contract(admin.clone());
    let token_client = token::Client::new(&env, &token_addr);

    let contract_id = env.register_contract(None, ExamplePaymentsContract);
    let client = ExamplePaymentsContractClient::new(&env, &contract_id);

    // Fee recipient — a non-admin address that receives fees.
    let fee_recipient = Address::generate(&env);

    client.initialize(&admin, &token_addr, &250, &fee_recipient);

    Fixture {
        env,
        admin,
        token_addr,
        token_client: unsafe { std::mem::transmute(token_client) },
        contract_id,
        client: unsafe { std::mem::transmute(client) },
    }
}

fn mint_tokens(env: &Env, token_addr: &Address, to: &Address, amount: i128) {
    let asset_client = token::StellarAssetClient::new(env, token_addr);
    asset_client.mint(to, &amount);
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

#[test]
fn initialize_sets_config() {
    let fx = setup();
    assert_eq!(fx.client.get_token(), Some(fx.token_addr.clone()));
    assert_eq!(fx.client.fee_rate(), 250);
    assert!(fx.client.fee_recipient().is_some());
}

// ---------------------------------------------------------------------------
// Deposits
// ---------------------------------------------------------------------------

#[test]
fn deposit_credits_balance() {
    let fx = setup();
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 1_000);

    fx.client.deposit(&user, &fx.token_addr, &500);

    assert_eq!(fx.client.balance_of(&user, &fx.token_addr), 500);
    assert_eq!(fx.client.total_deposits(&fx.token_addr), 500);
}

#[test]
fn deposit_multiple_times_accumulates() {
    let fx = setup();
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 2_000);

    fx.client.deposit(&user, &fx.token_addr, &300);
    fx.client.deposit(&user, &fx.token_addr, &200);

    assert_eq!(fx.client.balance_of(&user, &fx.token_addr), 500);
    assert_eq!(fx.client.total_deposits(&fx.token_addr), 500);
}

#[test]
fn deposit_rejects_zero_amount() {
    let fx = setup();
    let user = Address::generate(&fx.env);

    let result = fx.client.try_deposit(&user, &fx.token_addr, &0);
    assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
}

#[test]
fn deposit_rejects_negative_amount() {
    let fx = setup();
    let user = Address::generate(&fx.env);

    let result = fx.client.try_deposit(&user, &fx.token_addr, &-100);
    assert_eq!(result, Err(Ok(Error::PaymentInvalidAmount)));
}

#[test]
fn total_deposits_tracks_all_users() {
    let fx = setup();
    let user1 = Address::generate(&fx.env);
    let user2 = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user1, 1_000);
    mint_tokens(&fx.env, &fx.token_addr, &user2, 1_000);

    fx.client.deposit(&user1, &fx.token_addr, &300);
    fx.client.deposit(&user2, &fx.token_addr, &700);

    assert_eq!(fx.client.total_deposits(&fx.token_addr), 1_000);
}

#[test]
fn deposits_are_isolated_by_supported_token() {
    let fx = setup();
    let second_token = fx.env.register_stellar_asset_contract(fx.admin.clone());
    let second_client = token::Client::new(&fx.env, &second_token);
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 1_000);
    mint_tokens(&fx.env, &second_token, &user, 2_000);

    fx.client.add_supported_token(&fx.admin, &second_token);
    fx.client.deposit(&user, &fx.token_addr, &400);
    fx.client.deposit(&user, &second_token, &900);

    assert_eq!(fx.client.balance_of(&user, &fx.token_addr), 400);
    assert_eq!(fx.client.balance_of(&user, &second_token), 900);
    assert_eq!(fx.client.total_deposits(&fx.token_addr), 400);
    assert_eq!(fx.client.total_deposits(&second_token), 900);
    assert_eq!(second_client.balance(&fx.contract_id), 900);
}

#[test]
fn deposit_rejects_unsupported_token() {
    let fx = setup();
    let unsupported = fx.env.register_stellar_asset_contract(fx.admin.clone());
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &unsupported, &user, 1_000);

    let result = fx.client.try_deposit(&user, &unsupported, &500);
    assert_eq!(result, Err(Ok(Error::PaymentTokenNotSupported)));
}

// ---------------------------------------------------------------------------
// Withdrawals
// ---------------------------------------------------------------------------

#[test]
fn withdraw_sends_tokens_with_fee() {
    let fx = setup();
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 1_000);

    fx.client.deposit(&user, &fx.token_addr, &1_000);
    assert_eq!(fx.client.balance_of(&user, &fx.token_addr), 1_000);

    let fee_recipient = fx.client.fee_recipient().unwrap();
    let net = fx.client.withdraw(&user, &fx.token_addr);

    // 250 bps = 2.5% fee on 1_000 = 25. Net = 975.
    assert_eq!(net, 975);
    assert_eq!(fx.client.balance_of(&user, &fx.token_addr), 0);
    assert_eq!(fx.token_client.balance(&user), 975);
    assert_eq!(fx.token_client.balance(&fee_recipient), 25);
}

#[test]
fn withdraw_clears_total_deposits() {
    let fx = setup();
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 1_000);

    fx.client.deposit(&user, &fx.token_addr, &1_000);
    assert_eq!(fx.client.total_deposits(&fx.token_addr), 1_000);

    fx.client.withdraw(&user, &fx.token_addr);
    assert_eq!(fx.client.total_deposits(&fx.token_addr), 0);
}

#[test]
fn withdraw_rejects_zero_balance() {
    let fx = setup();
    let user = Address::generate(&fx.env);

    let result = fx.client.try_withdraw(&user, &fx.token_addr);
    assert_eq!(result, Err(Ok(Error::PaymentInsufficientBalance)));
}

#[test]
fn withdraw_amount_partial() {
    let fx = setup();
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 1_000);

    fx.client.deposit(&user, &fx.token_addr, &1_000);
    let net = fx.client.withdraw_amount(&user, &fx.token_addr, &400);

    // 2.5% fee on 400 = 10. Net = 390.
    assert_eq!(net, 390);
    assert_eq!(fx.client.balance_of(&user, &fx.token_addr), 600);
    assert_eq!(fx.client.total_deposits(&fx.token_addr), 600);
}

#[test]
fn withdraw_amount_rejects_over_balance() {
    let fx = setup();
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 1_000);

    fx.client.deposit(&user, &fx.token_addr, &500);
    let result = fx.client.try_withdraw_amount(&user, &fx.token_addr, &600);
    assert_eq!(result, Err(Ok(Error::PaymentInsufficientBalance)));
}

#[test]
fn settlement_uses_preferred_asset_when_reserve_is_available() {
    let fx = setup();
    let recipient = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &fx.contract_id, 500);

    let selected = fx.client.settle_with_fallback(
        &fx.admin, &recipient, &fx.token_addr, &fx.token_addr, &250,
    );

    assert_eq!(selected, fx.token_addr);
    assert_eq!(fx.token_client.balance(&recipient), 250);
}

#[test]
fn settlement_falls_back_when_preferred_reserve_is_insufficient() {
    let fx = setup();
    let fallback = fx.env.register_stellar_asset_contract(fx.admin.clone());
    let fallback_client = token::Client::new(&fx.env, &fallback);
    let recipient = Address::generate(&fx.env);
    fx.client.add_supported_token(&fx.admin, &fallback);
    mint_tokens(&fx.env, &fallback, &fx.contract_id, 600);

    let selected = fx.client.settle_with_fallback(
        &fx.admin, &recipient, &fx.token_addr, &fallback, &500,
    );

    assert_eq!(selected, fallback);
    assert_eq!(fallback_client.balance(&recipient), 500);
}

#[test]
fn settlement_fails_without_reserve_and_is_retryable_after_funding() {
    let fx = setup();
    let fallback = fx.env.register_stellar_asset_contract(fx.admin.clone());
    let recipient = Address::generate(&fx.env);
    fx.client.add_supported_token(&fx.admin, &fallback);

    let result = fx.client.try_settle_with_fallback(
        &fx.admin, &recipient, &fx.token_addr, &fallback, &500,
    );
    assert_eq!(result, Err(Ok(Error::PaymentInsufficientBalance)));

    mint_tokens(&fx.env, &fallback, &fx.contract_id, 500);
    assert_eq!(
        fx.client.settle_with_fallback(&fx.admin, &recipient, &fx.token_addr, &fallback, &500),
        fallback
    );
}

#[test]
fn settlement_requires_admin_authorization() {
    let fx = setup();
    let outsider = Address::generate(&fx.env);
    let recipient = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &fx.contract_id, 500);

    let result = fx.client.try_settle_with_fallback(
        &outsider, &recipient, &fx.token_addr, &fx.token_addr, &250,
    );
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

// ---------------------------------------------------------------------------
// Escrow
// ---------------------------------------------------------------------------

#[test]
fn escrow_create_and_release() {
    let fx = setup();
    let depositor = Address::generate(&fx.env);
    let beneficiary = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &depositor, 5_000);

    let expiry = fx.env.ledger().sequence() + 100;
    let escrow_id =
        fx.client
            .create_escrow_entry(&depositor, &beneficiary, &fx.token_addr, &2_000, &expiry);

    assert_eq!(escrow_id, 1);

    // Release
    fx.client.release_escrow_entry(&fx.admin, &escrow_id);
    assert_eq!(fx.token_client.balance(&beneficiary), 2_000);

    let record = fx.client.get_escrow_entry(&escrow_id).unwrap();
    assert_eq!(record.state, EscrowState::Released);
}

#[test]
fn escrow_create_and_refund() {
    let fx = setup();
    let depositor = Address::generate(&fx.env);
    let beneficiary = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &depositor, 5_000);

    let expiry = fx.env.ledger().sequence() + 100;
    let escrow_id =
        fx.client
            .create_escrow_entry(&depositor, &beneficiary, &fx.token_addr, &2_000, &expiry);

    fx.client.refund_escrow_entry(&fx.admin, &escrow_id);
    assert_eq!(fx.token_client.balance(&depositor), 5_000);

    let record = fx.client.get_escrow_entry(&escrow_id).unwrap();
    assert_eq!(record.state, EscrowState::Refunded);
}

#[test]
fn escrow_release_requires_admin() {
    let fx = setup();
    let depositor = Address::generate(&fx.env);
    let beneficiary = Address::generate(&fx.env);
    let non_admin = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &depositor, 5_000);

    let expiry = fx.env.ledger().sequence() + 100;
    let escrow_id =
        fx.client
            .create_escrow_entry(&depositor, &beneficiary, &fx.token_addr, &2_000, &expiry);

    let result = fx.client.try_release_escrow_entry(&non_admin, &escrow_id);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn escrow_release_after_expiry_fails() {
    let fx = setup();
    let depositor = Address::generate(&fx.env);
    let beneficiary = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &depositor, 5_000);

    let expiry = fx.env.ledger().sequence() + 5;
    let escrow_id =
        fx.client
            .create_escrow_entry(&depositor, &beneficiary, &fx.token_addr, &2_000, &expiry);

    // Advance past expiry
    fx.env.ledger().with_mut(|l| {
        l.sequence_number += 10;
    });

    let result = fx.client.try_release_escrow_entry(&fx.admin, &escrow_id);
    assert_eq!(result, Err(Ok(Error::PaymentEscrowExpired)));
}

// ---------------------------------------------------------------------------
// Fee Configuration
// ---------------------------------------------------------------------------

#[test]
fn set_fee_rate_updates_config() {
    let fx = setup();

    fx.client.set_fee_rate(&fx.admin, &500);
    assert_eq!(fx.client.fee_rate(), 500);
}

#[test]
fn set_fee_rate_rejects_out_of_range() {
    let fx = setup();

    let result = fx.client.try_set_fee_rate(&fx.admin, &10_001);
    assert_eq!(result, Err(Ok(Error::PaymentInvalidFeeRate)));
}

#[test]
fn set_fee_rate_requires_admin() {
    let fx = setup();
    let non_admin = Address::generate(&fx.env);

    let result = fx.client.try_set_fee_rate(&non_admin, &500);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

// ---------------------------------------------------------------------------
// Zero-Fee Withdrawal
// ---------------------------------------------------------------------------

#[test]
fn withdraw_with_zero_fee() {
    let fx = setup();
    let fee_recipient = fx.client.fee_recipient().unwrap();
    let user = Address::generate(&fx.env);
    mint_tokens(&fx.env, &fx.token_addr, &user, 1_000);

    // Set fee to 0
    fx.client.set_fee_rate(&fx.admin, &0);
    fx.client.deposit(&user, &fx.token_addr, &1_000);

    let net = fx.client.withdraw(&user, &fx.token_addr);
    assert_eq!(net, 1_000);
    assert_eq!(fx.token_client.balance(&user), 1_000);
    assert_eq!(fx.token_client.balance(&fee_recipient), 0);
}
