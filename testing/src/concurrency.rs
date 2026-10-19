//! Deterministic actor interleavings for testing race-prone contract mutations.
//!
//! Soroban applies ledger transactions serially. This harness lets a test
//! choose a repeatable interleaving of actor submissions and retries, while
//! the callback executes each attempt against the same live contract state.
//! It does not claim to create parallel ledger writes.

use alloc::vec::Vec;

/// One actor's view of a mutation before it is submitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationAttempt<Actor, Mutation> {
    pub actor: Actor,
    pub request_id: u64,
    pub expected_revision: u64,
    pub attempt: u32,
    pub mutation: Mutation,
}

/// A deterministic submission order shared by multiple simulated actors.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MultiActorConcurrencyHarness<Actor, Mutation> {
    attempts: Vec<MutationAttempt<Actor, Mutation>>,
}

impl<Actor, Mutation> MultiActorConcurrencyHarness<Actor, Mutation> {
    pub fn new() -> Self {
        Self {
            attempts: Vec::new(),
        }
    }

    pub fn queue(&mut self, attempt: MutationAttempt<Actor, Mutation>) {
        self.attempts.push(attempt);
    }

    /// Queue the original submission followed by retries of the same request.
    pub fn retry_storm(&mut self, attempt: MutationAttempt<Actor, Mutation>, retries: u32)
    where
        Actor: Clone,
        Mutation: Clone,
    {
        self.queue(attempt.clone());
        for retry in 1..=retries {
            let mut repeated = attempt.clone();
            repeated.attempt = repeated.attempt.saturating_add(retry);
            self.queue(repeated);
        }
    }

    pub fn attempts(&self) -> &[MutationAttempt<Actor, Mutation>] {
        &self.attempts
    }

    /// Apply each queued attempt in order to the same mutable state.
    pub fn run<Outcome>(
        &self,
        mut apply: impl FnMut(&MutationAttempt<Actor, Mutation>) -> Outcome,
    ) -> Vec<Outcome> {
        self.attempts.iter().map(&mut apply).collect()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{MultiActorConcurrencyHarness, MutationAttempt};
    use shared::errors::Error;
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, Ledger as _},
        token, Address, Env, Symbol,
    };
    use treasury_contract::{TimeWindow, TreasuryContract, TreasuryContractClient};

    #[derive(Debug, Eq, PartialEq)]
    enum AttemptResult {
        Applied,
        StaleSnapshot,
        Rejected(Error),
    }

    fn setup() -> (
        Env,
        TreasuryContractClient<'static>,
        Address,
        Address,
        Address,
        Symbol,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreasuryContract);
        let client = TreasuryContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let second_actor = Address::generate(&env);
        let token_address = env.register_stellar_asset_contract(admin.clone());
        let category = symbol_short!("reserve");

        client.initialize(&admin, &1_000);
        client.add_treasury_manager(&admin, &second_actor);
        token::StellarAssetClient::new(&env, &token_address).mint(&admin, &2_000);
        client.deposit(&admin, &token_address, &category, &2_000);

        (env, client, admin, second_actor, token_address, category)
    }

    fn schedule_withdrawal(
        client: &TreasuryContractClient,
        admin: &Address,
        token: &Address,
        recipient: &Address,
        action_id: &Symbol,
        amount: i128,
        not_before: u64,
        expires_at: u64,
    ) {
        client.schedule_withdraw(
            admin,
            action_id,
            token,
            recipient,
            &amount,
            &symbol_short!("reserve"),
            &TimeWindow {
                not_before,
                expires_at,
            },
        );
    }

    fn execute(
        client: &TreasuryContractClient,
        attempt: &MutationAttempt<Address, Symbol>,
    ) -> AttemptResult {
        match client.try_execute_scheduled_withdraw(&attempt.actor, &attempt.mutation) {
            Ok(Ok(())) => AttemptResult::Applied,
            Err(Ok(error)) => AttemptResult::Rejected(error),
            _ => AttemptResult::Rejected(Error::InvalidArgument),
        }
    }

    #[test]
    fn competing_actors_observe_stale_revision_without_overdrawing() {
        let (env, client, admin, second_actor, token, category) = setup();
        let recipient = Address::generate(&env);
        let first_action = Symbol::new(&env, "race_withdraw_a");
        let second_action = Symbol::new(&env, "race_withdraw_b");
        schedule_withdrawal(
            &client,
            &admin,
            &token,
            &recipient,
            &first_action,
            700,
            0,
            100,
        );
        schedule_withdrawal(
            &client,
            &admin,
            &token,
            &recipient,
            &second_action,
            700,
            0,
            100,
        );

        // Both actors prepared against revision 0. The harness serializes
        // their submissions and rejects the second stale snapshot before it
        // reaches the contract.
        let mut harness = MultiActorConcurrencyHarness::new();
        harness.queue(MutationAttempt {
            actor: admin,
            request_id: 10,
            expected_revision: 0,
            attempt: 0,
            mutation: first_action,
        });
        harness.queue(MutationAttempt {
            actor: second_actor,
            request_id: 11,
            expected_revision: 0,
            attempt: 0,
            mutation: second_action,
        });

        let mut revision = 0_u64;
        let outcomes = harness.run(|attempt| {
            if attempt.expected_revision != revision {
                return AttemptResult::StaleSnapshot;
            }
            let outcome = execute(&client, attempt);
            if outcome == AttemptResult::Applied {
                revision += 1;
            }
            outcome
        });

        assert_eq!(
            outcomes,
            [AttemptResult::Applied, AttemptResult::StaleSnapshot]
        );
        assert_eq!(client.category_balance(&token, &category), 1_300);
        assert_eq!(
            token::StellarAssetClient::new(&env, &token).balance(&recipient),
            700
        );
    }

    #[test]
    fn retry_storm_executes_a_scheduled_mutation_once() {
        let (env, client, admin, _second_actor, token, category) = setup();
        let recipient = Address::generate(&env);
        let action_id = Symbol::new(&env, "retry_withdraw");
        schedule_withdrawal(&client, &admin, &token, &recipient, &action_id, 100, 0, 100);

        let mut harness = MultiActorConcurrencyHarness::new();
        harness.retry_storm(
            MutationAttempt {
                actor: admin,
                request_id: 22,
                expected_revision: 0,
                attempt: 0,
                mutation: action_id,
            },
            4,
        );
        let outcomes = harness.run(|attempt| execute(&client, attempt));

        assert_eq!(outcomes[0], AttemptResult::Applied);
        assert_eq!(outcomes.len(), 5);
        assert!(outcomes[1..]
            .iter()
            .all(|outcome| { *outcome == AttemptResult::Rejected(Error::NotFound) }));
        assert_eq!(client.category_balance(&token, &category), 1_900);
        assert_eq!(
            token::StellarAssetClient::new(&env, &token).balance(&recipient),
            100
        );
    }

    #[test]
    fn stale_time_window_rejects_the_mutation_without_changing_balance() {
        let (env, client, admin, _second_actor, token, category) = setup();
        let recipient = Address::generate(&env);
        let action_id = Symbol::new(&env, "stale_withdraw");
        schedule_withdrawal(&client, &admin, &token, &recipient, &action_id, 100, 0, 5);
        env.ledger().set_timestamp(5);

        let mut harness = MultiActorConcurrencyHarness::new();
        harness.queue(MutationAttempt {
            actor: admin,
            request_id: 30,
            expected_revision: 0,
            attempt: 0,
            mutation: action_id,
        });
        let outcomes = harness.run(|attempt| execute(&client, attempt));

        assert_eq!(outcomes, [AttemptResult::Rejected(Error::ActionExpired)]);
        assert_eq!(client.category_balance(&token, &category), 2_000);
        assert_eq!(
            token::StellarAssetClient::new(&env, &token).balance(&recipient),
            0
        );
    }
}
