//! Canonical lifecycle event emission (Issue #144).
//!
//! Lifecycle-changing contract operations previously each invented their own
//! topic shape (`("aid", "created")`, `("record", "deact")`, `("state",
//! "trans")`, ...), so an indexer had to know every contract era and guess
//! which fields were present. This module defines a single, versioned
//! envelope that every lifecycle transition is emitted through:
//!
//! ```text
//! topics: ("lifecycle", <resource>, <transition>)
//! data:   LifecycleEvent { schema_version, resource, resource_id,
//!                          transition, from_state, to_state, actor, timestamp }
//! ```
//!
//! Properties indexers can rely on:
//!
//! - **Schema version** — every payload carries
//!   [`LIFECYCLE_EVENT_SCHEMA_VERSION`], so a future field addition is a
//!   version bump rather than a breaking decode.
//! - **Resource ID** — every payload carries the numeric ID of the record
//!   that transitioned, so events are joinable without scrolling the ledger.
//! - **Stable topics** — `("lifecycle", resource, transition)` is documented
//!   and asserted by tests; topics are bounded symbols, never addresses.
//! - **Deterministic order** — a workflow emits its transitions in causal
//!   order, and [`assert_lifecycle_sequence`] fails a test if any event is
//!   missing, extra, or reordered.
//!
//! These events are additive: contracts may keep emitting their legacy
//! per-module events (e.g. `AID_CREATED`) for backward compatibility while
//! migrating to the canonical envelope.
//!
//! ## Transition inventory
//!
//! | Resource | Transitions |
//! |---|---|
//! | `Aid` | `Created`, `Claimed`, `Settled`, `Refunded` |
//! | `Escrow` | `Created`, `Released`, `Refunded` |
//! | `Proposal` | `Created`, `Approved`, `Executed` |
//! | `ContractRecord` | `Activated`, `Deactivated` |
//!
//! See `docs/LIFECYCLE_EVENTS.md` for the full field contract and migration
//! guidance.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

/// Schema version stamped on every canonical lifecycle event.
///
/// Bump this whenever the payload shape changes; indexers pin the versions
/// they understand and can ignore (or backfill) the rest.
pub const LIFECYCLE_EVENT_SCHEMA_VERSION: u32 = 1;

/// Topic prefix shared by every canonical lifecycle event.
pub const LIFECYCLE_TOPIC: Symbol = symbol_short!("lifecycle");

/// State symbol used for a `Created` transition, which has no previous state.
pub const STATE_NONE: Symbol = symbol_short!("none");

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// The kind of record a lifecycle event describes.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleResource {
    /// Aid disbursement record.
    Aid,
    /// Payment escrow record.
    Escrow,
    /// Governance proposal.
    Proposal,
    /// Registered contract record (deprecation lifecycle).
    ContractRecord,
}

impl LifecycleResource {
    /// Stable topic symbol identifying this resource.
    pub fn as_symbol(self) -> Symbol {
        match self {
            LifecycleResource::Aid => symbol_short!("aid"),
            LifecycleResource::Escrow => symbol_short!("escrow"),
            LifecycleResource::Proposal => symbol_short!("proposal"),
            LifecycleResource::ContractRecord => symbol_short!("contract"),
        }
    }
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

/// A canonical lifecycle transition.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleTransition {
    /// Record was created (no previous state).
    Created,
    /// Aid was claimed by the recipient.
    Claimed,
    /// Aid was settled to the recipient.
    Settled,
    /// Aid / escrow was refunded to the originator.
    Refunded,
    /// Escrow was released to the payee.
    Released,
    /// Proposal was approved by an admin.
    Approved,
    /// Proposal was executed.
    Executed,
    /// Contract record was (re)activated.
    Activated,
    /// Contract record was deactivated.
    Deactivated,
}

impl LifecycleTransition {
    /// Stable topic symbol and terminal-state symbol for this transition.
    ///
    /// `Activated` maps to `active` and `Deactivated` to `deactive` so the
    /// canonical envelope stays consistent with `shared::lifecycle`'s
    /// `ContractRecordState` symbols.
    pub fn as_symbol(self) -> Symbol {
        match self {
            LifecycleTransition::Created => symbol_short!("created"),
            LifecycleTransition::Claimed => symbol_short!("claimed"),
            LifecycleTransition::Settled => symbol_short!("settled"),
            LifecycleTransition::Refunded => symbol_short!("refunded"),
            LifecycleTransition::Released => symbol_short!("released"),
            LifecycleTransition::Approved => symbol_short!("approved"),
            LifecycleTransition::Executed => symbol_short!("executed"),
            LifecycleTransition::Activated => symbol_short!("active"),
            LifecycleTransition::Deactivated => symbol_short!("deactive"),
        }
    }
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

/// Canonical payload published for every lifecycle transition.
///
/// The field set is fixed for a given [`LIFECYCLE_EVENT_SCHEMA_VERSION`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleEvent {
    /// Schema version of this payload (see [`LIFECYCLE_EVENT_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Resource kind that transitioned.
    pub resource: Symbol,
    /// Numeric ID of the record that transitioned.
    pub resource_id: u64,
    /// Transition that occurred.
    pub transition: Symbol,
    /// State before the transition (`STATE_NONE` for a creation).
    pub from_state: Symbol,
    /// State after the transition.
    pub to_state: Symbol,
    /// Address that triggered the transition.
    pub actor: Address,
    /// Ledger timestamp of the transition.
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

/// Publish a canonical lifecycle event.
///
/// Topics: `("lifecycle", resource, transition)`; data: [`LifecycleEvent`].
pub fn emit_lifecycle_transition(
    env: &Env,
    resource: Symbol,
    resource_id: u64,
    transition: Symbol,
    from_state: Symbol,
    to_state: Symbol,
    actor: &Address,
    timestamp: u64,
) {
    env.events().publish(
        (LIFECYCLE_TOPIC, resource.clone(), transition.clone()),
        LifecycleEvent {
            schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
            resource,
            resource_id,
            transition,
            from_state,
            to_state,
            actor: actor.clone(),
            timestamp,
        },
    );
}

/// Publish a canonical lifecycle event from typed resource/transition values.
pub fn emit_resource_transition(
    env: &Env,
    resource: LifecycleResource,
    resource_id: u64,
    transition: LifecycleTransition,
    from_state: Symbol,
    to_state: Symbol,
    actor: &Address,
    timestamp: u64,
) {
    emit_lifecycle_transition(
        env,
        resource.as_symbol(),
        resource_id,
        transition.as_symbol(),
        from_state,
        to_state,
        actor,
        timestamp,
    );
}

/// Emit an aid lifecycle event.
pub fn emit_aid_event(
    env: &Env,
    aid_id: u64,
    transition: LifecycleTransition,
    from_state: Symbol,
    to_state: Symbol,
    actor: &Address,
    timestamp: u64,
) {
    emit_resource_transition(
        env,
        LifecycleResource::Aid,
        aid_id,
        transition,
        from_state,
        to_state,
        actor,
        timestamp,
    );
}

/// Emit a payment escrow lifecycle event.
pub fn emit_escrow_event(
    env: &Env,
    escrow_id: u64,
    transition: LifecycleTransition,
    from_state: Symbol,
    to_state: Symbol,
    actor: &Address,
    timestamp: u64,
) {
    emit_resource_transition(
        env,
        LifecycleResource::Escrow,
        escrow_id,
        transition,
        from_state,
        to_state,
        actor,
        timestamp,
    );
}

/// Emit a governance proposal lifecycle event.
pub fn emit_proposal_event(
    env: &Env,
    proposal_id: u64,
    transition: LifecycleTransition,
    from_state: Symbol,
    to_state: Symbol,
    actor: &Address,
    timestamp: u64,
) {
    emit_resource_transition(
        env,
        LifecycleResource::Proposal,
        proposal_id,
        transition,
        from_state,
        to_state,
        actor,
        timestamp,
    );
}

/// Emit a contract-record (deactivation) lifecycle event.
pub fn emit_contract_record_event(
    env: &Env,
    record_id: u64,
    transition: LifecycleTransition,
    from_state: Symbol,
    to_state: Symbol,
    actor: &Address,
    timestamp: u64,
) {
    emit_resource_transition(
        env,
        LifecycleResource::ContractRecord,
        record_id,
        transition,
        from_state,
        to_state,
        actor,
        timestamp,
    );
}

// ---------------------------------------------------------------------------
// Ordered-sequence assertion
// ---------------------------------------------------------------------------

/// Assert that `observed` is exactly `expected`, in order.
///
/// Any missing event, extra event, or reordering panics with the index at
/// which the sequences first diverge. Contract tests should call this with the
/// decoded lifecycle events so that a dropped or reordered emission fails the
/// test instead of silently shipping.
pub fn assert_lifecycle_sequence(observed: &[LifecycleEvent], expected: &[LifecycleEvent]) {
    if observed.len() != expected.len() {
        panic!(
            "lifecycle event count mismatch: expected {}, observed {} (events missing or extra)",
            expected.len(),
            observed.len()
        );
    }
    let mut index = 0usize;
    while index < expected.len() {
        if observed[index] != expected[index] {
            panic!(
                "lifecycle event mismatch at index {}: expected {:?}, observed {:?} (missing, extra, or reordered)",
                index, expected[index], observed[index]
            );
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::testutils::{Address as _, Events};
    use soroban_sdk::{contract, contractimpl, symbol_short, FromVal, TryFromVal};
    use std::vec::Vec as StdVec;

    #[contract]
    struct LifecycleEventsFixture;

    #[contractimpl]
    impl LifecycleEventsFixture {
        pub fn noop(_env: Env) {}
    }

    /// Decode every canonical lifecycle payload recorded on the environment.
    fn decode_lifecycle(env: &Env) -> StdVec<LifecycleEvent> {
        let mut decoded = StdVec::new();
        for (_contract, topics, data) in env.events().all().iter() {
            if topics.is_empty() {
                continue;
            }
            let prefix: Symbol = Symbol::from_val(env, &topics.get(0).unwrap());
            if prefix == LIFECYCLE_TOPIC {
                decoded.push(LifecycleEvent::try_from_val(env, &data).unwrap());
            }
        }
        decoded
    }

    #[test]
    fn every_transition_carries_schema_version_and_resource_id() {
        let env = Env::default();
        let actor = Address::generate(&env);
        let contract_id = env.register_contract(None, LifecycleEventsFixture);

        env.as_contract(&contract_id, || {
            emit_aid_event(&env, 1, LifecycleTransition::Created, STATE_NONE, symbol_short!("pending"), &actor, 10);
            emit_aid_event(&env, 1, LifecycleTransition::Claimed, symbol_short!("pending"), symbol_short!("settled"), &actor, 11);
            emit_aid_event(&env, 1, LifecycleTransition::Settled, symbol_short!("pending"), symbol_short!("settled"), &actor, 12);
            emit_aid_event(&env, 1, LifecycleTransition::Refunded, symbol_short!("pending"), symbol_short!("refunded"), &actor, 13);
            emit_escrow_event(&env, 2, LifecycleTransition::Created, STATE_NONE, symbol_short!("active"), &actor, 14);
            emit_escrow_event(&env, 2, LifecycleTransition::Released, symbol_short!("active"), symbol_short!("released"), &actor, 15);
            emit_escrow_event(&env, 2, LifecycleTransition::Refunded, symbol_short!("active"), symbol_short!("refunded"), &actor, 16);
            emit_proposal_event(&env, 3, LifecycleTransition::Created, STATE_NONE, symbol_short!("pending"), &actor, 17);
            emit_proposal_event(&env, 3, LifecycleTransition::Approved, symbol_short!("pending"), symbol_short!("pending"), &actor, 18);
            emit_proposal_event(&env, 3, LifecycleTransition::Executed, symbol_short!("pending"), symbol_short!("executed"), &actor, 19);
            emit_contract_record_event(&env, 4, LifecycleTransition::Deactivated, symbol_short!("active"), symbol_short!("deactive"), &actor, 20);
            emit_contract_record_event(&env, 4, LifecycleTransition::Activated, symbol_short!("deactive"), symbol_short!("active"), &actor, 21);
        });

        let events = decode_lifecycle(&env);
        assert_eq!(events.len(), 12, "every lifecycle transition must emit an event");

        for event in events.iter() {
            assert_eq!(event.schema_version, LIFECYCLE_EVENT_SCHEMA_VERSION);
            assert!(event.resource_id > 0, "resource ID is required");
            assert_ne!(event.resource_id, 0);
        }

        // Resource IDs stay bound to their resource across transitions.
        assert_eq!(events[0].resource_id, 1);
        assert_eq!(events[10].resource_id, 4);
        assert_eq!(events[10].resource, LifecycleResource::ContractRecord.as_symbol());
    }

    #[test]
    fn topics_are_canonical_prefix_resource_and_transition() {
        let env = Env::default();
        let actor = Address::generate(&env);
        let contract_id = env.register_contract(None, LifecycleEventsFixture);

        env.as_contract(&contract_id, || {
            emit_aid_event(&env, 42, LifecycleTransition::Settled, symbol_short!("pending"), symbol_short!("settled"), &actor, 99);
        });

        let all = env.events().all();
        assert_eq!(all.len(), 1);
        let (_contract, topics, _data) = all.get(0).unwrap();
        assert_eq!(topics.len(), 3);
        assert_eq!(Symbol::from_val(&env, &topics.get(0).unwrap()), LIFECYCLE_TOPIC);
        assert_eq!(
            Symbol::from_val(&env, &topics.get(1).unwrap()),
            LifecycleResource::Aid.as_symbol()
        );
        assert_eq!(
            Symbol::from_val(&env, &topics.get(2).unwrap()),
            LifecycleTransition::Settled.as_symbol()
        );
    }

    #[test]
    fn aid_lifecycle_events_are_ordered() {
        let env = Env::default();
        let actor = Address::generate(&env);
        let contract_id = env.register_contract(None, LifecycleEventsFixture);

        env.as_contract(&contract_id, || {
            emit_aid_event(&env, 7, LifecycleTransition::Created, STATE_NONE, symbol_short!("pending"), &actor, 1);
            emit_aid_event(&env, 7, LifecycleTransition::Claimed, symbol_short!("pending"), symbol_short!("settled"), &actor, 2);
            emit_aid_event(&env, 7, LifecycleTransition::Settled, symbol_short!("pending"), symbol_short!("settled"), &actor, 3);
        });

        let observed = decode_lifecycle(&env);
        let pending = symbol_short!("pending");
        let settled = symbol_short!("settled");
        let expected = [
            LifecycleEvent {
                schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
                resource: LifecycleResource::Aid.as_symbol(),
                resource_id: 7,
                transition: LifecycleTransition::Created.as_symbol(),
                from_state: STATE_NONE,
                to_state: pending.clone(),
                actor: actor.clone(),
                timestamp: 1,
            },
            LifecycleEvent {
                schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
                resource: LifecycleResource::Aid.as_symbol(),
                resource_id: 7,
                transition: LifecycleTransition::Claimed.as_symbol(),
                from_state: pending.clone(),
                to_state: settled.clone(),
                actor: actor.clone(),
                timestamp: 2,
            },
            LifecycleEvent {
                schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
                resource: LifecycleResource::Aid.as_symbol(),
                resource_id: 7,
                transition: LifecycleTransition::Settled.as_symbol(),
                from_state: pending,
                to_state: settled,
                actor: actor.clone(),
                timestamp: 3,
            },
        ];

        assert_lifecycle_sequence(&observed, &expected);
    }

    #[test]
    #[should_panic(expected = "events missing or extra")]
    fn missing_event_fails_the_sequence_assertion() {
        let env = Env::default();
        let actor = Address::generate(&env);
        let contract_id = env.register_contract(None, LifecycleEventsFixture);

        env.as_contract(&contract_id, || {
            emit_aid_event(&env, 7, LifecycleTransition::Created, STATE_NONE, symbol_short!("pending"), &actor, 1);
            // Claimed is intentionally dropped: the sequence assertion must fail.
        });

        let observed = decode_lifecycle(&env);
        let expected = [
            LifecycleEvent {
                schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
                resource: LifecycleResource::Aid.as_symbol(),
                resource_id: 7,
                transition: LifecycleTransition::Created.as_symbol(),
                from_state: STATE_NONE,
                to_state: symbol_short!("pending"),
                actor: actor.clone(),
                timestamp: 1,
            },
            LifecycleEvent {
                schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
                resource: LifecycleResource::Aid.as_symbol(),
                resource_id: 7,
                transition: LifecycleTransition::Claimed.as_symbol(),
                from_state: symbol_short!("pending"),
                to_state: symbol_short!("settled"),
                actor: actor.clone(),
                timestamp: 2,
            },
        ];

        assert_lifecycle_sequence(&observed, &expected);
    }

    #[test]
    #[should_panic(expected = "missing, extra, or reordered")]
    fn reordered_event_fails_the_sequence_assertion() {
        let env = Env::default();
        let actor = Address::generate(&env);
        let contract_id = env.register_contract(None, LifecycleEventsFixture);

        env.as_contract(&contract_id, || {
            // Emit Settled before Claimed: same events, wrong order.
            emit_aid_event(&env, 7, LifecycleTransition::Settled, symbol_short!("pending"), symbol_short!("settled"), &actor, 2);
            emit_aid_event(&env, 7, LifecycleTransition::Claimed, symbol_short!("pending"), symbol_short!("settled"), &actor, 1);
        });

        let observed = decode_lifecycle(&env);
        let expected = [
            LifecycleEvent {
                schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
                resource: LifecycleResource::Aid.as_symbol(),
                resource_id: 7,
                transition: LifecycleTransition::Claimed.as_symbol(),
                from_state: symbol_short!("pending"),
                to_state: symbol_short!("settled"),
                actor: actor.clone(),
                timestamp: 1,
            },
            LifecycleEvent {
                schema_version: LIFECYCLE_EVENT_SCHEMA_VERSION,
                resource: LifecycleResource::Aid.as_symbol(),
                resource_id: 7,
                transition: LifecycleTransition::Settled.as_symbol(),
                from_state: symbol_short!("pending"),
                to_state: symbol_short!("settled"),
                actor: actor.clone(),
                timestamp: 2,
            },
        ];

        assert_lifecycle_sequence(&observed, &expected);
    }
}
