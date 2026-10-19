#!/usr/bin/env node
/**
 * Master verification runner for all 4 Trellis-contracts issues:
 * - Issue #296: Accessibility Coverage for Keyboard, Focus, and Async Status States
 * - Issue #297: Contributor Diagnostics for Environment and Dependency Validation
 * - Issue #298: Explainable Risk Scoring for Suspicious High-Impact Actions
 * - Issue #299: Release Readiness Checks for Migrations, Smoke Tests, and Rollback
 */

const { spawnSync } = require("node:child_process");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "..");

const suites = [
  {
    issue: "#296",
    title: "Accessibility Coverage (Keyboard, Focus, Async Status)",
    script: "scripts/verify-accessibility.cjs",
  },
  {
    issue: "#297",
    title: "Contributor Diagnostics (Environment, Dependencies, Fixtures)",
    script: "scripts/contributor-diagnostics.cjs",
  },
  {
    issue: "#298",
    title: "Explainable Risk Scoring (High-Impact Actions)",
    script: "scripts/verify-risk-scoring.cjs",
  },
  {
    issue: "#299",
    title: "Release Readiness Gate (Migrations, Smoke Tests, Rollback)",
    script: "scripts/verify-release-readiness.cjs",
  },
];

console.log("======================================================================");
console.log("TRELLIS-CONTRACTS: MASTER VERIFICATION RUNNER (#296, #297, #298, #299)");
console.log("======================================================================\n");

let allPassed = true;
const results = [];

for (const suite of suites) {
  console.log(`>>> Running Suite for Issue ${suite.issue}: ${suite.title} ...`);
  const scriptPath = path.join(ROOT, suite.script);
  const result = spawnSync(process.execPath, [scriptPath], {
    stdio: "inherit",
    env: process.env,
  });

  if (result.status === 0) {
    results.push({ issue: suite.issue, title: suite.title, status: "PASSED" });
  } else {
    results.push({ issue: suite.issue, title: suite.title, status: "FAILED" });
    allPassed = false;
  }
  console.log("");
}

console.log("======================================================================");
console.log("SUMMARY OF RESULTS:");
console.log("======================================================================");
for (const r of results) {
  const icon = r.status === "PASSED" ? "✓" : "✗";
  console.log(`  ${icon} [${r.status}] Issue ${r.issue}: ${r.title}`);
}
console.log("======================================================================");

if (!allPassed) {
  console.error("\nOne or more issue verification suites failed.");
  process.exit(1);
} else {
  console.log("\nAll 4 issues passed verification successfully!");
  process.exit(0);
}
