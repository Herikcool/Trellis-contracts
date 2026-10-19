//! Contributor Diagnostics for Environment and Dependency Validation (Issue #297).
//!
//! Provides automated verification of contributor development environments,
//! toolchain prerequisites, dependency version invariants (e.g. Soroban SDK 21.x,
//! Dalek cryptographic primitives), storage schema integrity, and fixture health.

use soroban_sdk::{
    contracterror, contracttype, symbol_short, Address, Env, Symbol, Vec,
};

use crate::auth::require_admin;
use crate::errors::Error;
use crate::storage::{persistent_get, persistent_set};

// ---------------------------------------------------------------------------
// Error Taxonomy (Range 970–979)
// ---------------------------------------------------------------------------

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DiagnosticError {
    /// Environment fails strict diagnostic validation criteria.
    DiagnosticValidationFailed = 970,
    /// Critical dependency invariant violated (version mismatch or unsupported pin).
    DependencyInvariantViolated = 971,
    /// Storage schema format or version mismatch.
    StorageSchemaInvalid = 972,
    /// Diagnostic checks list is empty.
    EmptyDiagnosticsList = 973,
    /// Caller unauthorized to record or clear diagnostic state.
    Unauthorized = 974,
}

impl From<DiagnosticError> for Error {
    fn from(err: DiagnosticError) -> Self {
        match err {
            DiagnosticError::DiagnosticValidationFailed => Error::ConfigInvalid,
            DiagnosticError::DependencyInvariantViolated => Error::ConfigInvalid,
            DiagnosticError::StorageSchemaInvalid => Error::ConfigInvalid,
            DiagnosticError::EmptyDiagnosticsList => Error::InvalidArgument,
            DiagnosticError::Unauthorized => Error::Unauthorized,
        }
    }
}

// ---------------------------------------------------------------------------
// Diagnostic Models & Enums
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DiagnosticCategory {
    Environment = 1,
    Toolchain = 2,
    DependencyInvariants = 3,
    StorageSchema = 4,
    WorkspaceFixtures = 5,
}

impl DiagnosticCategory {
    pub fn as_symbol(&self) -> Symbol {
        match self {
            DiagnosticCategory::Environment => symbol_short!("env"),
            DiagnosticCategory::Toolchain => symbol_short!("toolchain"),
            DiagnosticCategory::DependencyInvariants => symbol_short!("deps"),
            DiagnosticCategory::StorageSchema => symbol_short!("schema"),
            DiagnosticCategory::WorkspaceFixtures => symbol_short!("fixtures"),
        }
    }
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DiagnosticStatus {
    Passed = 1,
    Warning = 2,
    Failed = 3,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticCheck {
    pub component: Symbol,
    pub category: DiagnosticCategory,
    pub status: DiagnosticStatus,
    pub message: Symbol,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticReport {
    pub checks: Vec<DiagnosticCheck>,
    pub passed_count: u32,
    pub warning_count: u32,
    pub failed_count: u32,
    pub is_healthy: bool,
    pub evaluated_at: u64,
}

// ---------------------------------------------------------------------------
// Storage Keys
// ---------------------------------------------------------------------------

const KEY_LAST_DIAGNOSTIC: Symbol = symbol_short!("last_diag");

// ---------------------------------------------------------------------------
// Core Diagnostic Functions
// ---------------------------------------------------------------------------

/// Helper to create a single diagnostic check item.
pub fn create_diagnostic_check(
    component: Symbol,
    category: DiagnosticCategory,
    status: DiagnosticStatus,
    message: Symbol,
    timestamp: u64,
) -> DiagnosticCheck {
    DiagnosticCheck {
        component,
        category,
        status,
        message,
        timestamp,
    }
}

/// Evaluates a list of diagnostic checks.
///
/// In standard mode: passes if `failed_count == 0` (warnings allowed).
/// In strict mode: passes only if `failed_count == 0` AND `warning_count == 0`.
pub fn evaluate_diagnostics(
    _env: &Env,
    checks: &Vec<DiagnosticCheck>,
    strict_mode: bool,
    evaluated_at: u64,
) -> Result<DiagnosticReport, DiagnosticError> {
    if checks.is_empty() {
        return Err(DiagnosticError::EmptyDiagnosticsList);
    }

    let mut passed_count: u32 = 0;
    let mut warning_count: u32 = 0;
    let mut failed_count: u32 = 0;

    for i in 0..checks.len() {
        let check = checks.get(i).unwrap();
        match check.status {
            DiagnosticStatus::Passed => passed_count += 1,
            DiagnosticStatus::Warning => warning_count += 1,
            DiagnosticStatus::Failed => failed_count += 1,
        }
    }

    let is_healthy = if strict_mode {
        failed_count == 0 && warning_count == 0
    } else {
        failed_count == 0
    };

    let report = DiagnosticReport {
        checks: checks.clone(),
        passed_count,
        warning_count,
        failed_count,
        is_healthy,
        evaluated_at,
    };

    if !is_healthy {
        return Err(DiagnosticError::DiagnosticValidationFailed);
    }

    Ok(report)
}

/// Validates Soroban SDK and crypto library dependency version invariants.
pub fn validate_dependency_invariants(
    soroban_sdk_major: u32,
    _soroban_sdk_minor: u32,
    dalek_pinned: bool,
    timestamp: u64,
) -> Result<DiagnosticCheck, DiagnosticError> {
    // Protocol requires Soroban SDK 21.x and pinned Dalek primitives
    if soroban_sdk_major != 21 || !dalek_pinned {
        return Err(DiagnosticError::DependencyInvariantViolated);
    }

    Ok(create_diagnostic_check(
        symbol_short!("soroban"),
        DiagnosticCategory::DependencyInvariants,
        DiagnosticStatus::Passed,
        symbol_short!("dep_ok"),
        timestamp,
    ))
}

/// Records a system diagnostic report into persistent storage under admin authority.
pub fn record_system_diagnostic(
    env: &Env,
    admin: &Address,
    report: &DiagnosticReport,
) -> Result<(), Error> {
    require_admin(env, admin)?;

    persistent_set(env, &KEY_LAST_DIAGNOSTIC, report);

    env.events().publish(
        (symbol_short!("diag_rec"), report.is_healthy),
        (report.passed_count, report.warning_count, report.failed_count),
    );

    Ok(())
}

/// Retrieves the most recent system diagnostic report.
pub fn get_last_diagnostic_report(env: &Env) -> Option<DiagnosticReport> {
    persistent_get(env, &KEY_LAST_DIAGNOSTIC)
}

/// Runs a built-in diagnostic self-test to verify diagnostic pipeline functionality.
pub fn run_self_test(env: &Env) -> Result<DiagnosticReport, DiagnosticError> {
    let now = env.ledger().timestamp();
    let mut checks: Vec<DiagnosticCheck> = Vec::new(env);

    // 1. Toolchain self-check
    checks.push_back(create_diagnostic_check(
        symbol_short!("rustc"),
        DiagnosticCategory::Toolchain,
        DiagnosticStatus::Passed,
        symbol_short!("tool_ok"),
        now,
    ));

    // 2. Dependency invariant self-check
    let dep_check = validate_dependency_invariants(21, 7, true, now)?;
    checks.push_back(dep_check);

    // 3. Storage schema check
    checks.push_back(create_diagnostic_check(
        symbol_short!("storage"),
        DiagnosticCategory::StorageSchema,
        DiagnosticStatus::Passed,
        symbol_short!("sch_ok"),
        now,
    ));

    evaluate_diagnostics(env, &checks, false, now)
}
