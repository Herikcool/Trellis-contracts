//! Validation tests for the structured telemetry pipeline.
//!
//! These tests double as the issue's validation artifact: they assert that
//! telemetry fields exist and are populated for every one of the five core
//! operations, that success and failure are distinguishable, and that no
//! sensitive value reaches the event topics or payload.

extern crate std;

use soroban_sdk::testutils::{Events, Ledger};
use soroban_sdk::{contract, contractimpl, Env, FromVal, Symbol, TryFromVal};

use crate::telemetry::{
    emit_failure, emit_operation, emit_success, ledger_correlation, ActorType, TelemetryEvent,
    TelemetryResult, TelemetryTimer, CORE_OPERATIONS, OP_PAYMENT_TRANSFER, OP_QUOTA_CONSUME,
    OP_REBALANCE, TELEMETRY_TOPIC,
};
use crate::telemetry_protection::{
    is_safe_label, redact_label, sample_decision, SamplingConfig, MAX_LABEL_LEN,
};

#[contract]
pub struct TelemetryFixture;

#[contractimpl]
impl TelemetryFixture {
    pub fn noop(_env: Env) {}
}

/// Decode every telemetry payload recorded on the environment.
fn decode_telemetry(env: &Env) -> std::vec::Vec<TelemetryEvent> {
    let all = env.events().all();
    let mut decoded = std::vec::Vec::new();
    for (_contract, topics, data) in all.iter() {
        if topics.is_empty() {
            continue;
        }
        let prefix: Symbol = Symbol::from_val(env, &topics.get(0).unwrap());
        if prefix == TELEMETRY_TOPIC {
            decoded.push(TelemetryEvent::try_from_val(env, &data).unwrap());
        }
    }
    decoded
}

#[test]
fn all_core_operations_emit_the_five_structured_fields() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TelemetryFixture);
    let actors = [
        ActorType::User,
        ActorType::Admin,
        ActorType::Contract,
        ActorType::Worker,
        ActorType::System,
    ];

    env.as_contract(&contract_id, || {
        for (index, operation) in CORE_OPERATIONS.iter().enumerate() {
            emit_operation(
                &env,
                operation.clone(),
                actors[index].clone(),
                TelemetryResult::Success,
                index as u32,
                1_000 + index as u64,
            );
        }
    });

    let events = decode_telemetry(&env);
    assert_eq!(
        events.len(),
        CORE_OPERATIONS.len(),
        "every core operation must emit exactly one telemetry payload"
    );

    for (index, operation) in CORE_OPERATIONS.iter().enumerate() {
        let event = &events[index];
        assert_eq!(event.operation.clone(), operation.clone());
        assert_eq!(event.actor, actors[index]);
        assert_eq!(event.result, TelemetryResult::Success);
        assert_eq!(event.latency_ledgers, index as u32);
        assert_eq!(event.correlation_id, 1_000 + index as u64);
    }
}

#[test]
fn success_and_failure_payloads_are_distinguishable() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TelemetryFixture);

    env.as_contract(&contract_id, || {
        emit_success(&env, OP_QUOTA_CONSUME, ActorType::User, 0);
        emit_failure(&env, OP_QUOTA_CONSUME, ActorType::User, 0);
    });

    let events = decode_telemetry(&env);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].result, TelemetryResult::Success);
    assert_eq!(events[1].result, TelemetryResult::Failure);
    assert_eq!(events[0].operation, OP_QUOTA_CONSUME);
    assert_eq!(events[1].operation, OP_QUOTA_CONSUME);
}

#[test]
fn timer_reports_elapsed_ledgers_and_explicit_correlation_id() {
    let env = Env::default();
    env.ledger().set_sequence_number(100);
    let contract_id = env.register_contract(None, TelemetryFixture);

    env.as_contract(&contract_id, || {
        let timer = TelemetryTimer::start(&env, OP_REBALANCE, ActorType::Contract, 77);
        env.ledger().set_sequence_number(103);
        assert_eq!(timer.latency_ledgers(&env), 3);
        timer.succeed(&env);
    });

    let events = decode_telemetry(&env);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].latency_ledgers, 3);
    assert_eq!(events[0].correlation_id, 77);
    assert_eq!(events[0].result, TelemetryResult::Success);
}

#[test]
fn default_correlation_id_is_the_ledger_sequence() {
    let env = Env::default();
    env.ledger().set_sequence_number(4_242);
    let contract_id = env.register_contract(None, TelemetryFixture);

    env.as_contract(&contract_id, || {
        emit_success(&env, OP_QUOTA_CONSUME, ActorType::System, 0);
    });

    let events = decode_telemetry(&env);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].correlation_id, 4_242);
    assert_eq!(ledger_correlation(&env), 4_242);
}

#[test]
fn payload_topics_are_bounded_and_sensitive_free() {
    let env = Env::default();
    let contract_id = env.register_contract(None, TelemetryFixture);

    env.as_contract(&contract_id, || {
        emit_success(&env, OP_PAYMENT_TRANSFER, ActorType::User, 0);
    });

    let all = env.events().all();
    assert_eq!(all.len(), 1);
    let (_contract, topics, data) = all.get(0).unwrap();

    // Topics are exactly ("tel", operation): no address and no free-form value.
    assert_eq!(topics.len(), 2);
    let prefix: Symbol = Symbol::from_val(&env, &topics.get(0).unwrap());
    let suffix: Symbol = Symbol::from_val(&env, &topics.get(1).unwrap());
    assert_eq!(prefix, TELEMETRY_TOPIC);
    assert_eq!(suffix, OP_PAYMENT_TRANSFER);

    // The payload decodes to the fixed five-field struct (no addresses/amounts).
    let event = TelemetryEvent::try_from_val(&env, &data).unwrap();
    assert_eq!(event.operation, OP_PAYMENT_TRANSFER);
    assert_eq!(event.actor, ActorType::User);
    assert_eq!(event.result, TelemetryResult::Success);
    assert_eq!(event.latency_ledgers, 0);
    assert_eq!(event.correlation_id, env.ledger().sequence() as u64);
}

#[test]
fn unsafe_labels_are_rejected_and_sensitive_values_redacted() {
    // High-cardinality / sensitive labels must be rejected outright.
    assert!(!is_safe_label("user:GABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRSTUV"));
    assert!(!is_safe_label("amount:1234567890"));
    assert!(!is_safe_label(""));
    assert!(!is_safe_label(&"x".repeat(MAX_LABEL_LEN + 1)));

    // Safe, bounded labels pass through unchanged.
    assert!(is_safe_label("op:payment_transfer"));
    assert!(is_safe_label("actor:user"));

    // Redaction replaces sensitive values with a stable placeholder.
    let redacted = redact_label("user:GABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRSTUV");
    assert_eq!(redacted, "user:<redacted>");
    let redacted_amount = redact_label("amount:1234567890");
    assert_eq!(redacted_amount, "amount:<redacted>");
}

#[test]
fn sampling_keeps_dashboards_useful_under_abusive_volume() {
    let config = SamplingConfig::new(10, 100);

    // Under the burst threshold, every event is kept.
    for i in 0..10u32 {
        assert!(sample_decision(&config, i, 0));
    }

    // Above the threshold, sampling kicks in deterministically.
    let kept: u32 = (0..100u32)
        .filter(|i| sample_decision(&config, *i, 1_000))
        .count() as u32;
    assert!(kept > 0, "sampling must not drop all events");
    assert!(kept < 100, "sampling must reduce abusive volume");

    // Errors are always retained regardless of volume.
    assert!(sample_decision(&config, 0, 1_000_000));
}
