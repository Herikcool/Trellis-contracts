//! Safe, contract-visible service status for operators and public clients.
//!
//! Status reasons are a closed enum rather than caller-provided text, so a
//! public status update cannot accidentally publish secrets or internal logs.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

use crate::auth::require_admin;
use crate::errors::Error;

/// Public availability state. `Unknown` is returned until an operator reports
/// a state; consumers must not interpret missing data as healthy.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicStatus {
    Unknown,
    Operational,
    Degraded,
    Paused,
    Maintenance,
}

/// Fixed, non-sensitive explanations suitable for public status surfaces.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicStatusReason {
    NotReported,
    Normal,
    DependencyUnavailable,
    ProcessingDelay,
    ElevatedFailures,
    EmergencyPause,
    ScheduledPause,
    ScheduledMaintenance,
    UpgradeInProgress,
}

/// Snapshot of public availability. The ledger sequence gives consumers a
/// deterministic freshness marker without exposing wall-clock or private data.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicStatusRecord {
    pub status: PublicStatus,
    pub reason: PublicStatusReason,
    pub updated_at_ledger: u32,
}

const KEY_PUBLIC_STATUS: Symbol = symbol_short!("pub_stat");

/// Read the current public status. An unset status is explicitly `Unknown`.
pub fn get_public_status(env: &Env) -> PublicStatusRecord {
    env.storage()
        .instance()
        .get(&KEY_PUBLIC_STATUS)
        .unwrap_or(PublicStatusRecord {
            status: PublicStatus::Unknown,
            reason: PublicStatusReason::NotReported,
            updated_at_ledger: 0,
        })
}

/// Set a public status using a bounded reason code.
///
/// Only the configured admin may update the status. Invalid status/reason
/// combinations are rejected so clients can rely on the documented mapping.
pub fn set_public_status(
    env: &Env,
    admin: &Address,
    status: PublicStatus,
    reason: PublicStatusReason,
) -> Result<PublicStatusRecord, Error> {
    require_admin(env, admin)?;
    if !valid_reason(status, reason) {
        return Err(Error::InvalidArgument);
    }

    let record = PublicStatusRecord {
        status,
        reason,
        updated_at_ledger: env.ledger().sequence(),
    };
    env.storage().instance().set(&KEY_PUBLIC_STATUS, &record);
    env.events().publish((symbol_short!("pub_stat"),), record);
    Ok(record)
}

fn valid_reason(status: PublicStatus, reason: PublicStatusReason) -> bool {
    match status {
        PublicStatus::Unknown => false,
        PublicStatus::Operational => reason == PublicStatusReason::Normal,
        PublicStatus::Degraded => matches!(
            reason,
            PublicStatusReason::DependencyUnavailable
                | PublicStatusReason::ProcessingDelay
                | PublicStatusReason::ElevatedFailures
        ),
        PublicStatus::Paused => matches!(
            reason,
            PublicStatusReason::EmergencyPause | PublicStatusReason::ScheduledPause
        ),
        PublicStatus::Maintenance => matches!(
            reason,
            PublicStatusReason::ScheduledMaintenance | PublicStatusReason::UpgradeInProgress
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    #[test]
    fn unset_status_is_unknown_and_safe() {
        let env = Env::default();
        let record = get_public_status(&env);
        assert_eq!(record.status, PublicStatus::Unknown);
        assert_eq!(record.reason, PublicStatusReason::NotReported);
        assert_eq!(record.updated_at_ledger, 0);
    }

    #[test]
    fn admin_can_publish_status_transitions_and_recover() {
        let env = Env::default();
        let contract = env.register_contract(None, TestContract);
        env.as_contract(&contract, || {
            let admin = Address::generate(&env);
            crate::auth::set_admin(&env, &admin);
            env.mock_all_auths();

            let maintenance = set_public_status(
                &env,
                &admin,
                PublicStatus::Maintenance,
                PublicStatusReason::ScheduledMaintenance,
            )
            .unwrap();
            assert_eq!(get_public_status(&env), maintenance);

            let degraded = set_public_status(
                &env,
                &admin,
                PublicStatus::Degraded,
                PublicStatusReason::DependencyUnavailable,
            )
            .unwrap();
            assert_eq!(get_public_status(&env), degraded);

            let recovered = set_public_status(
                &env,
                &admin,
                PublicStatus::Operational,
                PublicStatusReason::Normal,
            )
            .unwrap();
            assert_eq!(get_public_status(&env), recovered);
            assert!(recovered.updated_at_ledger >= maintenance.updated_at_ledger);
        });
    }

    #[test]
    fn rejects_mismatched_reasons_and_non_admin_callers() {
        let env = Env::default();
        let contract = env.register_contract(None, TestContract);
        env.as_contract(&contract, || {
            let admin = Address::generate(&env);
            crate::auth::set_admin(&env, &admin);
            env.mock_all_auths();

            assert_eq!(
                set_public_status(
                    &env,
                    &admin,
                    PublicStatus::Operational,
                    PublicStatusReason::EmergencyPause,
                ),
                Err(Error::InvalidArgument)
            );
            let other = Address::generate(&env);
            assert_eq!(
                set_public_status(
                    &env,
                    &other,
                    PublicStatus::Paused,
                    PublicStatusReason::EmergencyPause,
                ),
                Err(Error::Unauthorized)
            );
            assert_eq!(get_public_status(&env).status, PublicStatus::Unknown);
        });
    }

    #[soroban_sdk::contract]
    struct TestContract;

    #[soroban_sdk::contractimpl]
    impl TestContract {}
}
