#!/usr/bin/env node
/**
 * Automated Test Suite for Trellis Operator Backfill Tool (Issue #217).
 *
 * Verifies:
 * - Dry-run diff generation with zero side-effects.
 * - Resumable checkpoints across batches.
 * - Recovery from interrupted runs without duplicate side-effects.
 * - Detailed classification of created, updated, skipped, and failed records.
 * - Rollback functionality using reverse mutation journals.
 * - All modules: Aid, Treasury, Referral, Governance, Oracle, and Marketplace.
 *
 * Usage:
 *   node scripts/test-backfill.cjs
 */
"use strict";

const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const REPO_ROOT = path.resolve(__dirname, "..");
const BACKFILL_CLI = path.join(REPO_ROOT, "scripts", "backfill.cjs");
const TEMP_DIR = path.join(REPO_ROOT, ".test-backfill-scratch");

let passedTests = 0;
let failedTests = 0;

function assert(condition, testName, details = "") {
  if (condition) {
    passedTests++;
    console.log(`  ✓ ${testName}`);
  } else {
    failedTests++;
    console.error(`  ✗ FAIL: ${testName} ${details ? `(${details})` : ""}`);
  }
}

function runCli(args) {
  const res = spawnSync(process.execPath, [BACKFILL_CLI, ...args], {
    cwd: REPO_ROOT,
    encoding: "utf8",
  });
  return {
    code: res.status,
    stdout: res.stdout || "",
    stderr: res.stderr || "",
  };
}

function cleanTempDir() {
  if (fs.existsSync(TEMP_DIR)) {
    fs.rmSync(TEMP_DIR, { recursive: true, force: true });
  }
  fs.mkdirSync(TEMP_DIR, { recursive: true });
}

console.log("=== Running Trellis Operator Backfill Test Suite ===\n");
cleanTempDir();

// ---------------------------------------------------------------------------
// 1. Dry-run Mode Verification (Zero mutations & accurate diff)
// ---------------------------------------------------------------------------
console.log("[Test Suite 1] Dry-Run Diff & Previews");
{
  const targetStateFile = path.join(TEMP_DIR, "state-dryrun.json");
  const checkpointFile = path.join(TEMP_DIR, "cp-dryrun.json");
  const inputFile = path.join(REPO_ROOT, "fixtures", "backfill", "aid.json");

  const res = runCli([
    "--module", "aid",
    "--input", inputFile,
    "--target-state", targetStateFile,
    "--checkpoint", checkpointFile,
    "--dry-run",
    "--json"
  ]);

  assert(res.code === 0, "Dry-run exits with code 0");
  assert(!fs.existsSync(targetStateFile), "Dry-run makes NO writes to target state file");
  assert(!fs.existsSync(checkpointFile), "Dry-run creates NO checkpoint file");

  let parsed = {};
  try {
    parsed = JSON.parse(res.stdout);
  } catch (e) {
    // parse error
  }

  assert(parsed.dryRun === true, "Report identifies dryRun: true");
  assert(parsed.totalScanned === 4, "Scans all 4 input records");
  assert(parsed.toCreate === 3, "Identifies 3 valid records to create");
  assert(parsed.failedCount === 1, "Detects 1 invalid record (amount 0)");
  assert(parsed.failedRecords[0].reason.includes("Amount must be positive"), "Reports exact validation failure reason");
}

// ---------------------------------------------------------------------------
// 2. Apply Mode & Resumable Checkpoint Creation
// ---------------------------------------------------------------------------
console.log("\n[Test Suite 2] Batch Apply & Checkpoints");
{
  const targetStateFile = path.join(TEMP_DIR, "state-apply.json");
  const checkpointFile = path.join(TEMP_DIR, "cp-apply.json");
  const inputFile = path.join(REPO_ROOT, "fixtures", "backfill", "aid.json");

  const res = runCli([
    "--module", "aid",
    "--input", inputFile,
    "--target-state", targetStateFile,
    "--checkpoint", checkpointFile,
    "--batch-size", "2",
    "--mode", "best-effort",
    "--apply",
    "--json"
  ]);

  assert(res.code === 0, "Best-effort apply exits with code 0");
  assert(fs.existsSync(targetStateFile), "Target state file is created");
  assert(fs.existsSync(checkpointFile), "Checkpoint file is persisted");

  const state = JSON.parse(fs.readFileSync(targetStateFile, "utf8"));
  const cp = JSON.parse(fs.readFileSync(checkpointFile, "utf8"));

  assert(cp.status === "Completed", "Checkpoint status is Completed");
  assert(cp.stats.created === 3, "Persisted 3 valid records");
  assert(cp.stats.failed === 1, "Recorded 1 failed record");
  assert(state["1"] && state["2"] && state["3"], "Records with IDs 1, 2, 3 exist in state");
  assert(!state["4"], "Invalid record 4 was not written to state");
  assert(cp.rollbackJournal.length === 3, "Rollback journal recorded 3 entries");
}

// ---------------------------------------------------------------------------
// 3. Idempotent Re-runs (Skipping identical records)
// ---------------------------------------------------------------------------
console.log("\n[Test Suite 3] Idempotency & Skip Classification");
{
  const targetStateFile = path.join(TEMP_DIR, "state-apply.json");
  const checkpointFile = path.join(TEMP_DIR, "cp-rerun.json");
  const inputFile = path.join(REPO_ROOT, "fixtures", "backfill", "aid.json");

  // Run dry-run against already applied state
  const res = runCli([
    "--module", "aid",
    "--input", inputFile,
    "--target-state", targetStateFile,
    "--checkpoint", checkpointFile,
    "--dry-run",
    "--json"
  ]);

  const parsed = JSON.parse(res.stdout);
  assert(parsed.toCreate === 0, "No new records to create on replay");
  assert(parsed.toSkip === 3, "All 3 existing valid records skipped as AlreadyUpToDate");
  assert(parsed.skippedRecords[0].reason === "AlreadyUpToDate", "Skip reason is AlreadyUpToDate");
}

// ---------------------------------------------------------------------------
// 4. Interruption & Resumption (All-or-Nothing Recovery)
// ---------------------------------------------------------------------------
console.log("\n[Test Suite 4] Failure Interruption & Resumable Recovery");
{
  const targetStateFile = path.join(TEMP_DIR, "state-resume.json");
  const checkpointFile = path.join(TEMP_DIR, "cp-resume.json");

  // Create input with error on 3rd item
  const testInput = [
    { id: 10, donor: "GAAA", recipient: "GBBB", amount: "100", expiry: 1800000000, status: "Pending", schema_version: 2 },
    { id: 11, donor: "GAAA", recipient: "GBBB", amount: "200", expiry: 1800000000, status: "Pending", schema_version: 2 },
    { id: 12, donor: "GAAA", recipient: "GBBB", amount: "0", expiry: 1800000000, status: "Pending", schema_version: 2 }, // FAILS
    { id: 13, donor: "GAAA", recipient: "GBBB", amount: "400", expiry: 1800000000, status: "Pending", schema_version: 2 },
  ];
  const inputFilePath = path.join(TEMP_DIR, "interrupted-input.json");
  fs.writeFileSync(inputFilePath, JSON.stringify(testInput, null, 2), "utf8");

  // Run with all-or-nothing: should pause at record 12
  const run1 = runCli([
    "--module", "aid",
    "--input", inputFilePath,
    "--target-state", targetStateFile,
    "--checkpoint", checkpointFile,
    "--batch-size", "2",
    "--mode", "all-or-nothing",
    "--apply",
    "--json"
  ]);

  assert(run1.code === 1, "Exits non-zero on failure under all-or-nothing mode");
  const cp1 = JSON.parse(fs.readFileSync(checkpointFile, "utf8"));
  assert(cp1.status === "AwaitingRecovery", "Checkpoint status set to AwaitingRecovery");
  assert(cp1.cursor === 2, "Checkpoint cursor accurately set to index 2");
  assert(cp1.stats.created === 2, "First 2 records were created");

  // Fix the failing record and resume
  testInput[2].amount = "300";
  fs.writeFileSync(inputFilePath, JSON.stringify(testInput, null, 2), "utf8");

  const run2 = runCli([
    "--module", "aid",
    "--input", inputFilePath,
    "--target-state", targetStateFile,
    "--checkpoint", checkpointFile,
    "--mode", "all-or-nothing",
    "--resume",
    "--json"
  ]);

  assert(run2.code === 0, "Resume succeeds with exit code 0");
  const cp2 = JSON.parse(fs.readFileSync(checkpointFile, "utf8"));
  assert(cp2.status === "Completed", "Checkpoint advanced to Completed");
  assert(cp2.stats.created === 4, "Total 4 records created across the runs");
}

// ---------------------------------------------------------------------------
// 5. Rollback Recovery
// ---------------------------------------------------------------------------
console.log("\n[Test Suite 5] Rollback Functionality");
{
  const targetStateFile = path.join(TEMP_DIR, "state-rollback.json");
  const checkpointFile = path.join(TEMP_DIR, "cp-rollback.json");

  // Start with existing state
  const initialState = {
    "100": { id: 100, donor: "GAAA", recipient: "GBBB", amount: "50", expiry: 1800000000, status: "Pending", schema_version: 1 }
  };
  fs.writeFileSync(targetStateFile, JSON.stringify(initialState, null, 2), "utf8");

  // Backfill dataset that updates 100 and creates 101
  const updateInput = [
    { id: 100, donor: "GAAA", recipient: "GBBB", amount: "500", expiry: 1800000000, status: "Pending", schema_version: 2 },
    { id: 101, donor: "GAAA", recipient: "GBBB", amount: "600", expiry: 1800000000, status: "Pending", schema_version: 2 },
  ];
  const inputFilePath = path.join(TEMP_DIR, "rollback-input.json");
  fs.writeFileSync(inputFilePath, JSON.stringify(updateInput, null, 2), "utf8");

  // Apply
  runCli([
    "--module", "aid",
    "--input", inputFilePath,
    "--target-state", targetStateFile,
    "--checkpoint", checkpointFile,
    "--apply",
    "--json"
  ]);

  const mutatedState = JSON.parse(fs.readFileSync(targetStateFile, "utf8"));
  assert(mutatedState["100"].amount === "500" && mutatedState["101"], "Changes applied to target state");

  // Rollback
  const rollbackRes = runCli([
    "--module", "aid",
    "--checkpoint", checkpointFile,
    "--target-state", targetStateFile,
    "--rollback",
    "--json"
  ]);

  assert(rollbackRes.code === 0, "Rollback exits with code 0");
  const restoredState = JSON.parse(fs.readFileSync(targetStateFile, "utf8"));
  assert(restoredState["100"].amount === "50", "Updated record 100 restored to initial amount 50");
  assert(restoredState["100"].schema_version === 1, "Schema version restored to 1");
  assert(!restoredState["101"], "Created record 101 removed from state");

  const cpRollback = JSON.parse(fs.readFileSync(checkpointFile, "utf8"));
  assert(cpRollback.status === "RolledBack", "Checkpoint status marked RolledBack");
}

// ---------------------------------------------------------------------------
// 6. Multi-Module Validation Verification
// ---------------------------------------------------------------------------
console.log("\n[Test Suite 6] Multi-Module Domain Constraints");

// Treasury
{
  const res = runCli([
    "--module", "treasury",
    "--input", path.join(REPO_ROOT, "fixtures", "backfill", "treasury.json"),
    "--target-state", path.join(TEMP_DIR, "state-treasury.json"),
    "--dry-run",
    "--json"
  ]);
  const parsed = JSON.parse(res.stdout);
  assert(parsed.toCreate === 3, "Treasury: 3 valid balances recognized");
  assert(parsed.failedCount === 1, "Treasury: negative balance detected and rejected");
}

// Referral
{
  const res = runCli([
    "--module", "referral",
    "--input", path.join(REPO_ROOT, "fixtures", "backfill", "referral.json"),
    "--target-state", path.join(TEMP_DIR, "state-referral.json"),
    "--dry-run",
    "--json"
  ]);
  const parsed = JSON.parse(res.stdout);
  assert(parsed.toCreate === 2, "Referral: 2 valid mappings recognized");
  assert(parsed.failedCount === 1, "Referral: self-referral detected and rejected");
}

// Governance
{
  const res = runCli([
    "--module", "governance",
    "--input", path.join(REPO_ROOT, "fixtures", "backfill", "governance.json"),
    "--target-state", path.join(TEMP_DIR, "state-gov.json"),
    "--dry-run",
    "--json"
  ]);
  const parsed = JSON.parse(res.stdout);
  assert(parsed.toCreate === 4, "Governance: parameters and admin set recognized");
}

// Oracle
{
  const res = runCli([
    "--module", "oracle",
    "--input", path.join(REPO_ROOT, "fixtures", "backfill", "oracle.json"),
    "--target-state", path.join(TEMP_DIR, "state-oracle.json"),
    "--dry-run",
    "--json"
  ]);
  const parsed = JSON.parse(res.stdout);
  assert(parsed.toCreate === 3, "Oracle: price feeds recognized");
}

// Marketplace
{
  const res = runCli([
    "--module", "marketplace",
    "--input", path.join(REPO_ROOT, "fixtures", "backfill", "marketplace.json"),
    "--target-state", path.join(TEMP_DIR, "state-market.json"),
    "--dry-run",
    "--json"
  ]);
  const parsed = JSON.parse(res.stdout);
  assert(parsed.toCreate === 2, "Marketplace: collections recognized");
}

// ---------------------------------------------------------------------------
// 7. Error Handling & CLI Edge Cases
// ---------------------------------------------------------------------------
console.log("\n[Test Suite 7] CLI Edge Cases & Malformed Inputs");
{
  const resNoModule = runCli(["--input", "fixtures/backfill/aid.json"]);
  assert(resNoModule.code === 2, "Missing --module exits with code 2");

  const resBadModule = runCli(["--module", "unknown_module", "--input", "fixtures/backfill/aid.json"]);
  assert(resBadModule.code === 2, "Invalid --module exits with code 2");

  const resNoInput = runCli(["--module", "aid"]);
  assert(resNoInput.code === 2, "Missing --input exits with code 2");

  const emptyInputPath = path.join(TEMP_DIR, "empty.json");
  fs.writeFileSync(emptyInputPath, "[]", "utf8");
  const resEmpty = runCli(["--module", "aid", "--input", emptyInputPath]);
  assert(resEmpty.code === 2, "Empty records array exits with code 2");
}

// Clean up scratch directory
cleanTempDir();

console.log("\n------------------------------------------------------------");
console.log(`Test Execution Finished: ${passedTests} passed, ${failedTests} failed.`);
console.log("------------------------------------------------------------\n");

process.exit(failedTests === 0 ? 0 : 1);
