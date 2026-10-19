#!/usr/bin/env node
/**
 * Executable verification script for Issue #299:
 * Release Readiness Checks for Migrations, Smoke Tests, and Rollback.
 */

const assert = require("node:assert/strict");

const GateKind = {
  Migration: "Migration",
  Security: "Security",
  SmokeTest: "SmokeTest",
  RollbackRehearsal: "RollbackRehearsal",
};

const RollbackStrategy = {
  Revert: "Revert",
  ForwardFix: "ForwardFix",
  Manual: "Manual",
};

function verifyMigrationReadiness(fromVersion, toVersion, hasDryRunJournal, unmigratedRecords) {
  if (toVersion <= fromVersion) {
    return { ok: false, error: "Target version must be strictly greater than current version" };
  }
  if (!hasDryRunJournal) {
    return { ok: false, error: "Dry-run migration journal is required before release promotion" };
  }
  if (unmigratedRecords > 0) {
    return { ok: false, error: `Found ${unmigratedRecords} unmigrated records; migration incomplete` };
  }
  return { ok: true, gate: GateKind.Migration, status: "Passed" };
}

function verifySmokeTests(rpcHealthy, getterLiveness, clientBindingIntact) {
  if (!rpcHealthy) {
    return { ok: false, error: "Soroban RPC probe failed or timed out" };
  }
  if (!getterLiveness) {
    return { ok: false, error: "Contract public getter liveness check failed" };
  }
  if (!clientBindingIntact) {
    return { ok: false, error: "Cross-contract client bindings drift detected" };
  }
  return { ok: true, gate: GateKind.SmokeTest, status: "Passed" };
}

function verifyRollbackReadiness(strategy, snapshotStored, rehearsalPassed) {
  if (!snapshotStored) {
    return { ok: false, error: "Pre-upgrade state snapshot missing; cannot guarantee rollback safety" };
  }
  if (!rehearsalPassed) {
    return { ok: false, error: "Rollback rehearsal simulation failed state consistency checks" };
  }
  if (strategy === RollbackStrategy.Manual) {
    return { ok: false, error: "Manual rollback strategy cannot guarantee automated recovery" };
  }
  return { ok: true, gate: GateKind.RollbackRehearsal, status: "Passed" };
}

function evaluateFullRelease(manifest) {
  const issues = [];
  const requiredGates = [GateKind.Migration, GateKind.Security, GateKind.SmokeTest, GateKind.RollbackRehearsal];

  for (const mod of manifest.modules) {
    for (const gate of requiredGates) {
      const check = manifest.checks.find((c) => c.module === mod && c.gate === gate);
      if (!check) {
        if (!manifest.exceptionMarker) {
          issues.push(`Module ${mod} is missing mandatory gate: ${gate}`);
        }
      } else if (check.status !== "Passed") {
        issues.push(`Module ${mod} gate ${gate} failed: ${check.error}`);
      }
    }
  }

  return {
    isReady: issues.length === 0,
    issues,
    releaseVersion: manifest.version,
  };
}

function runVerification() {
  console.log("==================================================================");
  console.log("TRELLIS: RELEASE READINESS VERIFICATION RUNNER (#299)");
  console.log("==================================================================");

  // Test 1: Migration Verification (Normal and Failure paths)
  console.log("\n[1/5] Testing Migration Gate Verification...");
  const migOk = verifyMigrationReadiness(1, 2, true, 0);
  assert.equal(migOk.ok, true);
  console.log("  ✓ Migration passed with valid version order and 0 unmigrated records");

  const migFailUnmigrated = verifyMigrationReadiness(1, 2, true, 5);
  assert.equal(migFailUnmigrated.ok, false);
  console.log("  ✓ Caught failure on unmigrated records: " + migFailUnmigrated.error);

  const migFailNoJournal = verifyMigrationReadiness(1, 2, false, 0);
  assert.equal(migFailNoJournal.ok, false);
  console.log("  ✓ Caught failure on missing dry-run journal");

  // Test 2: Smoke Tests Verification
  console.log("\n[2/5] Testing Smoke Tests Gate Verification...");
  const smokeOk = verifySmokeTests(true, true, true);
  assert.equal(smokeOk.ok, true);
  console.log("  ✓ Smoke tests passed across RPC, getter liveness, and client bindings");

  const smokeFailRpc = verifySmokeTests(false, true, true);
  assert.equal(smokeFailRpc.ok, false);
  console.log("  ✓ Caught failure on RPC reachability: " + smokeFailRpc.error);

  // Test 3: Rollback Readiness Verification
  console.log("\n[3/5] Testing Rollback Rehearsal Gate Verification...");
  const rollbackOk = verifyRollbackReadiness(RollbackStrategy.Revert, true, true);
  assert.equal(rollbackOk.ok, true);
  console.log("  ✓ Rollback rehearsal passed with stored snapshot and Revert strategy");

  const rollbackFailManual = verifyRollbackReadiness(RollbackStrategy.Manual, true, true);
  assert.equal(rollbackFailManual.ok, false);
  console.log("  ✓ Caught failure on Manual rollback strategy: " + rollbackFailManual.error);

  // Test 4: Complete Release Manifest Evaluation (Passing)
  console.log("\n[4/5] Testing Full Protocol Manifest Evaluation (All 4 Gates)...");
  const modules = ["Treasury", "Aid", "Referral", "Governance", "Oracle", "Marketplace"];
  const checks = [];
  for (const m of modules) {
    checks.push({ module: m, gate: GateKind.Migration, status: "Passed" });
    checks.push({ module: m, gate: GateKind.Security, status: "Passed" });
    checks.push({ module: m, gate: GateKind.SmokeTest, status: "Passed" });
    checks.push({ module: m, gate: GateKind.RollbackRehearsal, status: "Passed" });
  }

  const manifest = { version: "v2.0.0", modules, checks, exceptionMarker: null };
  const res = evaluateFullRelease(manifest);
  assert.equal(res.isReady, true);
  assert.equal(res.issues.length, 0);
  console.log("  ✓ Full release manifest ready across all 6 core modules (24 checks passed)");

  // Test 5: Missing Gate Blocking
  console.log("\n[5/5] Testing Missing Gate Blocking...");
  const incompleteManifest = {
    version: "v2.0.1",
    modules: ["Treasury"],
    checks: [{ module: "Treasury", gate: GateKind.Migration, status: "Passed" }],
    exceptionMarker: null,
  };
  const resBlocked = evaluateFullRelease(incompleteManifest);
  assert.equal(resBlocked.isReady, false);
  assert.equal(resBlocked.issues.length, 3);
  console.log("  ✓ Incomplete release correctly blocked with 3 missing gate errors");

  console.log("\n==================================================================");
  console.log("ALL RELEASE READINESS GATE VERIFICATIONS PASSED (#299)");
  console.log("==================================================================");
}

runVerification();
