//! Privacy-preserving audit evidence bundles for support investigations (Issue #218).
//!
//! Provides exportable audit evidence bundles that redact secrets and PII
//! while preserving cryptographic hashes, timestamps, module identifiers, and
//! correlation IDs necessary for verifying system behavior during dispute
//! resolution and investigations.

use soroban_sdk::{
    contracttype, Address, Bytes, BytesN, Env, IntoVal, Symbol, Val, Vec,
};
use soroban_sdk::xdr::ToXdr;

/// Redaction levels for audit evidence bundles.
pub const REDACTION_NONE: u32 = 0;
pub const REDACTION_SUPPORT: u32 = 1;
pub const REDACTION_STRICT: u32 = 2;

/// A single privacy-preserving audit evidence record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceRecord {
    pub record_id: u64,
    pub timestamp: u64,
    /// Pseudonymized or redacted actor digest.
    pub actor_digest: BytesN<32>,
    pub module: Symbol,
    pub action: Symbol,
    pub correlation_id: BytesN<32>,
    /// Fingerprint of payload parameters with PII/secrets redacted.
    pub payload_hash: BytesN<32>,
    pub status_code: u32,
    /// Cryptographic integrity hash over this record's canonical fields.
    pub integrity_hash: BytesN<32>,
}

/// An exportable, verifiable bundle of evidence records for a support investigation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEvidenceBundle {
    pub bundle_id: u64,
    pub created_at: u64,
    pub ticket_id: Symbol,
    pub redaction_level: u32,
    pub record_count: u32,
    /// Root hash verifying all records in the bundle.
    pub root_digest: BytesN<32>,
    pub records: Vec<EvidenceRecord>,
}

/// Raw input data for building an evidence record prior to redaction.
#[derive(Clone, Debug)]
pub struct RawEvidenceInput {
    pub record_id: u64,
    pub timestamp: u64,
    pub actor: Address,
    pub module: Symbol,
    pub action: Symbol,
    pub correlation_id: BytesN<32>,
    pub sensitive_payload: Bytes,
    pub status_code: u32,
}

/// Redact/pseudonymize actor address according to redaction level and salt.
pub fn redact_actor(env: &Env, actor: &Address, salt: &BytesN<32>, level: u32) -> BytesN<32> {
    let mut b = Bytes::new(env);
    b.append(&actor.clone().to_xdr(env));
    if level != REDACTION_NONE {
        let salt_bytes: Bytes = salt.clone().into();
        b.append(&salt_bytes);
    }
    env.crypto().sha256(&b).into()
}

/// Hash sensitive payload with salt to protect PII while preserving verification.
pub fn hash_sensitive_payload(env: &Env, payload: &Bytes, salt: &BytesN<32>) -> BytesN<32> {
    let mut b = Bytes::new(env);
    b.append(payload);
    let salt_bytes: Bytes = salt.clone().into();
    b.append(&salt_bytes);
    env.crypto().sha256(&b).into()
}

/// Compute cryptographic integrity hash for an evidence record.
pub fn compute_record_hash(
    env: &Env,
    record_id: u64,
    timestamp: u64,
    actor_digest: &BytesN<32>,
    module: &Symbol,
    action: &Symbol,
    correlation_id: &BytesN<32>,
    payload_hash: &BytesN<32>,
    status_code: u32,
) -> BytesN<32> {
    let mut parts: Vec<Val> = Vec::new(env);
    parts.push_back(record_id.into_val(env));
    parts.push_back(timestamp.into_val(env));
    parts.push_back(actor_digest.clone().into_val(env));
    parts.push_back(module.clone().into_val(env));
    parts.push_back(action.clone().into_val(env));
    parts.push_back(correlation_id.clone().into_val(env));
    parts.push_back(payload_hash.clone().into_val(env));
    parts.push_back(status_code.into_val(env));
    env.crypto().sha256(&parts.to_xdr(env)).into()
}

/// Compute aggregate root digest across all records in the bundle.
pub fn compute_bundle_root_hash(env: &Env, records: &Vec<EvidenceRecord>) -> BytesN<32> {
    if records.is_empty() {
        return BytesN::from_array(env, &[0u8; 32]);
    }
    let mut accumulator: Bytes = Bytes::new(env);
    for r in records.iter() {
        let h_bytes: Bytes = r.integrity_hash.into();
        accumulator.append(&h_bytes);
    }
    env.crypto().sha256(&accumulator).into()
}

/// Construct a privacy-preserving evidence bundle from raw inputs.
pub fn create_evidence_bundle(
    env: &Env,
    bundle_id: u64,
    ticket_id: Symbol,
    redaction_level: u32,
    salt: &BytesN<32>,
    raw_records: &[RawEvidenceInput],
) -> AuditEvidenceBundle {
    let mut records = Vec::new(env);
    for raw in raw_records {
        let actor_digest = redact_actor(env, &raw.actor, salt, redaction_level);
        let payload_hash = hash_sensitive_payload(env, &raw.sensitive_payload, salt);
        let integrity_hash = compute_record_hash(
            env,
            raw.record_id,
            raw.timestamp,
            &actor_digest,
            &raw.module,
            &raw.action,
            &raw.correlation_id,
            &payload_hash,
            raw.status_code,
        );
        records.push_back(EvidenceRecord {
            record_id: raw.record_id,
            timestamp: raw.timestamp,
            actor_digest,
            module: raw.module.clone(),
            action: raw.action.clone(),
            correlation_id: raw.correlation_id.clone(),
            payload_hash,
            status_code: raw.status_code,
            integrity_hash,
        });
    }

    let root_digest = compute_bundle_root_hash(env, &records);
    let record_count = records.len();

    AuditEvidenceBundle {
        bundle_id,
        created_at: env.ledger().timestamp(),
        ticket_id,
        redaction_level,
        record_count,
        root_digest,
        records,
    }
}

/// Verify cryptographic integrity of an audit evidence bundle.
pub fn verify_bundle(env: &Env, bundle: &AuditEvidenceBundle) -> bool {
    if bundle.records.len() != bundle.record_count {
        return false;
    }

    for r in bundle.records.iter() {
        let expected_hash = compute_record_hash(
            env,
            r.record_id,
            r.timestamp,
            &r.actor_digest,
            &r.module,
            &r.action,
            &r.correlation_id,
            &r.payload_hash,
            r.status_code,
        );
        if r.integrity_hash != expected_hash {
            return false;
        }
    }

    let expected_root = compute_bundle_root_hash(env, &bundle.records);
    bundle.root_digest == expected_root
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::symbol_short;
    use soroban_sdk::testutils::Address as _;

    fn dummy_salt(env: &Env) -> BytesN<32> {
        BytesN::from_array(env, &[7u8; 32])
    }

    fn dummy_cid(env: &Env, id: u8) -> BytesN<32> {
        let mut arr = [0u8; 32];
        arr[0] = id;
        BytesN::from_array(env, &arr)
    }

    #[test]
    fn test_bundle_creation_and_verification() {
        let env = Env::default();
        let salt = dummy_salt(&env);
        let actor1 = Address::generate(&env);
        let actor2 = Address::generate(&env);

        let mut payload1 = Bytes::new(&env);
        payload1.append(&Bytes::from_slice(&env, b"pii_data_ssn_12345"));

        let mut payload2 = Bytes::new(&env);
        payload2.append(&Bytes::from_slice(&env, b"secret_transfer_recipient"));

        let inputs = [
            RawEvidenceInput {
                record_id: 1,
                timestamp: 1000,
                actor: actor1,
                module: symbol_short!("aid"),
                action: symbol_short!("create"),
                correlation_id: dummy_cid(&env, 1),
                sensitive_payload: payload1,
                status_code: 0,
            },
            RawEvidenceInput {
                record_id: 2,
                timestamp: 1050,
                actor: actor2,
                module: symbol_short!("nft"),
                action: symbol_short!("bid"),
                correlation_id: dummy_cid(&env, 2),
                sensitive_payload: payload2,
                status_code: 0,
            },
        ];

        let bundle = create_evidence_bundle(
            &env,
            101,
            symbol_short!("TICK_42"),
            REDACTION_SUPPORT,
            &salt,
            &inputs,
        );

        assert_eq!(bundle.bundle_id, 101);
        assert_eq!(bundle.record_count, 2);
        assert_eq!(bundle.redaction_level, REDACTION_SUPPORT);
        assert!(verify_bundle(&env, &bundle));
    }

    #[test]
    fn test_tampered_record_fails_verification() {
        let env = Env::default();
        let salt = dummy_salt(&env);
        let actor = Address::generate(&env);

        let inputs = [RawEvidenceInput {
            record_id: 1,
            timestamp: 1000,
            actor,
            module: symbol_short!("aid"),
            action: symbol_short!("claim"),
            correlation_id: dummy_cid(&env, 1),
            sensitive_payload: Bytes::from_slice(&env, b"secret"),
            status_code: 0,
        }];

        let mut bundle = create_evidence_bundle(
            &env,
            202,
            symbol_short!("TICK_99"),
            REDACTION_STRICT,
            &salt,
            &inputs,
        );

        assert!(verify_bundle(&env, &bundle));

        // Tamper with action in record 0
        let mut tampered_records = Vec::new(&env);
        let mut rec0 = bundle.records.get(0).unwrap();
        rec0.action = symbol_short!("refund");
        tampered_records.push_back(rec0);
        bundle.records = tampered_records;

        assert!(!verify_bundle(&env, &bundle));
    }

    #[test]
    fn test_tampered_root_fails_verification() {
        let env = Env::default();
        let salt = dummy_salt(&env);
        let actor = Address::generate(&env);

        let inputs = [RawEvidenceInput {
            record_id: 1,
            timestamp: 1000,
            actor,
            module: symbol_short!("aid"),
            action: symbol_short!("claim"),
            correlation_id: dummy_cid(&env, 1),
            sensitive_payload: Bytes::from_slice(&env, b"secret"),
            status_code: 0,
        }];

        let mut bundle = create_evidence_bundle(
            &env,
            303,
            symbol_short!("TICK_100"),
            REDACTION_SUPPORT,
            &salt,
            &inputs,
        );

        // Tamper with root digest
        bundle.root_digest = dummy_cid(&env, 99);
        assert!(!verify_bundle(&env, &bundle));
    }

    #[test]
    fn test_empty_bundle_handling() {
        let env = Env::default();
        let salt = dummy_salt(&env);
        let inputs: [RawEvidenceInput; 0] = [];

        let bundle = create_evidence_bundle(
            &env,
            404,
            symbol_short!("EMPTY"),
            REDACTION_STRICT,
            &salt,
            &inputs,
        );

        assert_eq!(bundle.record_count, 0);
        assert!(verify_bundle(&env, &bundle));
    }
}
