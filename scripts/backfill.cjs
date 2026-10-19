#!/usr/bin/env node
/**
 * Trellis Operator-Safe Backfill Tool (Issue #217).
 *
 * Provides safe historical data and schema backfilling across Trellis Soroban
 * contract modules: Treasury, Aid, Referral, Governance, Oracle, and Marketplace.
 *
 * Guarantees:
 * - Read-only dry-run diff preview before any state mutation.
 * - Resumable checkpoints recorded per batch to prevent duplicate side effects.
 * - Explicit classification and reporting of created, updated, skipped, and failed records.
 * - Automated reverse rollback journal for rapid incident mitigation.
 * - Zero external runtime dependencies (vanilla Node.js).
 *
 * Usage:
 *   node scripts/backfill.cjs --module <mod> --input <file> [options]
 *
 * Options:
 *   --module <name>          aid | treasury | referral | governance | oracle | marketplace | all
 *   --input <file>           Path to JSON backfill dataset
 *   --dry-run                Preview changes and diffs without writing (default)
 *   --apply                  Commit changes to target state
 *   --checkpoint <file>      Custom checkpoint file (default: .backfill-checkpoint-<module>.json)
 *   --resume                 Resume an interrupted run from existing checkpoint
 *   --rollback               Revert mutations using the checkpoint rollback journal
 *   --status                 Print current checkpoint status
 *   --clear-checkpoint       Clear an existing checkpoint after operator review
 *   --batch-size <n>         Records per checkpoint commit (1..100, default: 25)
 *   --mode <mode>            all-or-nothing | best-effort (default: best-effort)
 *   --duplicate-policy <pol> skip | update | reject (default: skip)
 *   --target-state <file>    Path to ledger/mock state file (default: .state-<module>.json)
 *   --force                  Bypass confirmation prompts
 *   --json                   Output machine-readable report
 *   --help                   Display this help message
 *
 * Exit codes: 0 = success, 1 = validation/execution failures, 2 = usage error.
 */
"use strict";

const fs = require("fs");
const path = require("path");

const SUPPORTED_MODULES = ["aid", "treasury", "referral", "governance", "oracle", "marketplace", "all"];
const DEFAULT_BATCH_SIZE = 25;
const MAX_BATCH_SIZE = 100;

function printUsage() {
  console.log(`Trellis Operator-Safe Backfill Tool

Usage:
  node scripts/backfill.cjs --module <name> --input <file> [options]

Commands & Modes:
  --dry-run                Preview diffs without mutating state (default)
  --apply                  Apply backfill changes in batches
  --resume                 Resume previous run from checkpoint
  --rollback               Roll back changes recorded in checkpoint journal
  --status                 Display checkpoint status and statistics
  --clear-checkpoint       Clear checkpoint file

Configuration:
  --module <name>          aid | treasury | referral | governance | oracle | marketplace | all
  --input <file>           Input records JSON file
  --checkpoint <file>      Override checkpoint filepath
  --target-state <file>    Path to state snapshot JSON file
  --batch-size <n>         Batch commit size (1..100, default: ${DEFAULT_BATCH_SIZE})
  --mode <mode>            all-or-nothing | best-effort (default: best-effort)
  --duplicate-policy <pol> skip | update | reject (default: skip)
  --force                  Skip confirmation prompt
  --json                   Output JSON formatted summary
  --help                   Show help message
`);
}

function die(msg, fix, code = 2) {
  console.error(`BACKFILL ERROR: ${msg}`);
  if (fix) console.error(`  Remediation: ${fix}`);
  process.exit(code);
}

function parseArgs(argv) {
  const args = {
    module: null,
    input: null,
    dryRun: true,
    apply: false,
    resume: false,
    rollback: false,
    status: false,
    clearCheckpoint: false,
    checkpoint: null,
    targetState: null,
    batchSize: DEFAULT_BATCH_SIZE,
    mode: "best-effort",
    duplicatePolicy: "skip",
    force: false,
    json: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    switch (arg) {
      case "--module":
        args.module = argv[++i];
        break;
      case "--input":
        args.input = argv[++i];
        break;
      case "--dry-run":
        args.dryRun = true;
        args.apply = false;
        break;
      case "--apply":
        args.apply = true;
        args.dryRun = false;
        break;
      case "--resume":
        args.resume = true;
        args.dryRun = false;
        args.apply = true;
        break;
      case "--rollback":
        args.rollback = true;
        args.dryRun = false;
        break;
      case "--status":
        args.status = true;
        break;
      case "--clear-checkpoint":
        args.clearCheckpoint = true;
        break;
      case "--checkpoint":
        args.checkpoint = argv[++i];
        break;
      case "--target-state":
        args.targetState = argv[++i];
        break;
      case "--batch-size":
        args.batchSize = parseInt(argv[++i], 10);
        break;
      case "--mode":
        args.mode = argv[++i];
        break;
      case "--duplicate-policy":
        args.duplicatePolicy = argv[++i];
        break;
      case "--force":
      case "--yes":
        args.force = true;
        break;
      case "--json":
        args.json = true;
        break;
      case "--help":
      case "-h":
        printUsage();
        process.exit(0);
        break;
      default:
        die(`Unknown argument "${arg}"`, "Run with --help for supported options.");
    }
  }

  return args;
}

// ---------------------------------------------------------------------------
// Module-specific validation rules and key extractors
// ---------------------------------------------------------------------------

function extractRecordKey(mod, record) {
  switch (mod) {
    case "aid":
      return record.id != null ? String(record.id) : null;
    case "treasury":
      if (record.token && record.category) {
        return `${record.token}:${record.category}`;
      }
      return null;
    case "referral":
      return record.referee != null ? String(record.referee) : null;
    case "governance":
      if (record.type === "parameter") return `param:${record.key}`;
      if (record.type === "admin_set") return "admin_set";
      return record.key || record.id || null;
    case "oracle":
      return record.asset != null ? String(record.asset) : null;
    case "marketplace":
      if (record.collection_id != null) return `col:${record.collection_id}`;
      if (record.listing_id != null) return `list:${record.listing_id}`;
      return null;
    default:
      return record.id != null ? String(record.id) : null;
  }
}

function validateRecord(mod, record) {
  if (!record || typeof record !== "object") {
    return { valid: false, reason: "Record must be a JSON object" };
  }

  switch (mod) {
    case "aid": {
      if (record.id == null || isNaN(Number(record.id))) {
        return { valid: false, reason: "Missing or invalid numeric 'id'" };
      }
      if (!record.donor || typeof record.donor !== "string") {
        return { valid: false, reason: "Missing or invalid 'donor' address" };
      }
      if (!record.recipient || typeof record.recipient !== "string") {
        return { valid: false, reason: "Missing or invalid 'recipient' address" };
      }
      const amt = BigInt(record.amount || "0");
      if (amt <= 0n) {
        return { valid: false, reason: `Amount must be positive, got ${record.amount}` };
      }
      if (!record.expiry || Number(record.expiry) <= 0) {
        return { valid: false, reason: "Missing or non-positive expiration timestamp" };
      }
      if (record.schema_version != null && record.schema_version !== 2) {
        return { valid: false, reason: `Target schema version must be 2, got ${record.schema_version}` };
      }
      return { valid: true };
    }

    case "treasury": {
      if (!record.token || typeof record.token !== "string") {
        return { valid: false, reason: "Missing 'token' address" };
      }
      if (!record.category || typeof record.category !== "string") {
        return { valid: false, reason: "Missing category symbol" };
      }
      if (record.balance != null) {
        const bal = BigInt(record.balance);
        if (bal < 0n) {
          return { valid: false, reason: `Balance cannot be negative, got ${record.balance}` };
        }
      }
      return { valid: true };
    }

    case "referral": {
      if (!record.referee || typeof record.referee !== "string") {
        return { valid: false, reason: "Missing 'referee' address" };
      }
      if (!record.referrer || typeof record.referrer !== "string") {
        return { valid: false, reason: "Missing 'referrer' address" };
      }
      if (record.referee === record.referrer) {
        return { valid: false, reason: "Self-referral invariant violation (referee == referrer)" };
      }
      if (record.lifetime_accrued != null) {
        const acc = BigInt(record.lifetime_accrued);
        if (acc < 0n) {
          return { valid: false, reason: "Lifetime accrued commission cannot be negative" };
        }
      }
      return { valid: true };
    }

    case "governance": {
      if (record.type === "parameter") {
        if (!record.key || typeof record.key !== "string") {
          return { valid: false, reason: "Parameter record missing 'key'" };
        }
        if (record.value == null) {
          return { valid: false, reason: "Parameter record missing 'value'" };
        }
      } else if (record.type === "admin_set") {
        if (!Array.isArray(record.admins) || record.admins.length === 0) {
          return { valid: false, reason: "Admin set must contain a non-empty array of addresses" };
        }
        if (record.admins.length > 20) {
          return { valid: false, reason: `Admin set cannot exceed 20 signers, got ${record.admins.length}` };
        }
        if (!record.threshold || record.threshold < 1 || record.threshold > record.admins.length) {
          return { valid: false, reason: `Threshold must be between 1 and ${record.admins.length}` };
        }
        if (record.quorum_bps != null && (record.quorum_bps < 0 || record.quorum_bps > 10000)) {
          return { valid: false, reason: "Quorum BPS must be between 0 and 10000" };
        }
      } else {
        return { valid: false, reason: `Unknown governance record type: ${record.type}` };
      }
      return { valid: true };
    }

    case "oracle": {
      if (!record.asset || typeof record.asset !== "string") {
        return { valid: false, reason: "Missing oracle 'asset' identifier" };
      }
      const price = BigInt(record.price || "0");
      if (price <= 0n) {
        return { valid: false, reason: `Oracle price must be positive, got ${record.price}` };
      }
      if (!record.timestamp || Number(record.timestamp) <= 0) {
        return { valid: false, reason: "Missing or non-positive timestamp" };
      }
      return { valid: true };
    }

    case "marketplace": {
      if (record.collection_id != null) {
        if (!record.name || !record.symbol || !record.owner) {
          return { valid: false, reason: "Collection missing required name, symbol, or owner" };
        }
        if (record.royalty_bps != null && (record.royalty_bps < 0 || record.royalty_bps > 10000)) {
          return { valid: false, reason: "Royalty BPS must be between 0 and 10000" };
        }
        if (record.metadata_hash && String(record.metadata_hash).length !== 64) {
          return { valid: false, reason: "Collection metadata_hash must be a 64-char hex string (32 bytes)" };
        }
      } else if (record.listing_id != null) {
        if (!record.price || BigInt(record.price) <= 0n) {
          return { valid: false, reason: "Listing price must be positive" };
        }
      } else {
        return { valid: false, reason: "Marketplace record must be either a collection or listing" };
      }
      return { valid: true };
    }

    default:
      return { valid: true };
  }
}

// ---------------------------------------------------------------------------
// Diff calculation
// ---------------------------------------------------------------------------

function areValuesEqual(a, b) {
  if (a === b) return true;
  if (a == null || b == null) return false;
  if (typeof a === "object" && typeof b === "object") {
    return JSON.stringify(a) === JSON.stringify(b);
  }
  return String(a) === String(b);
}

function computeDiff(oldVal, newVal) {
  if (!oldVal) {
    return { action: "CREATE", changes: newVal };
  }

  const changes = {};
  let hasChanges = false;

  const allKeys = new Set([...Object.keys(oldVal), ...Object.keys(newVal)]);
  for (const k of allKeys) {
    if (!areValuesEqual(oldVal[k], newVal[k])) {
      changes[k] = { old: oldVal[k], new: newVal[k] };
      hasChanges = true;
    }
  }

  if (!hasChanges) {
    return { action: "SKIP", reason: "AlreadyUpToDate" };
  }

  return { action: "UPDATE", changes };
}

function formatDiffLine(key, change) {
  if (change.old === undefined) {
    return `    + ${key}: ${JSON.stringify(change.new)}`;
  } else if (change.new === undefined) {
    return `    - ${key}: ${JSON.stringify(change.old)}`;
  } else {
    return `    ~ ${key}: ${JSON.stringify(change.old)} -> ${JSON.stringify(change.new)}`;
  }
}

// ---------------------------------------------------------------------------
// Checkpoint & State Management
// ---------------------------------------------------------------------------

function getCheckpointPath(mod, customPath) {
  if (customPath) return path.resolve(customPath);
  return path.resolve(`.backfill-checkpoint-${mod}.json`);
}

function getStatePath(mod, customPath) {
  if (customPath) return path.resolve(customPath);
  return path.resolve(`.state-${mod}.json`);
}

function loadJsonFile(filepath, fallback = null) {
  if (!fs.existsSync(filepath)) return fallback;
  try {
    return JSON.parse(fs.readFileSync(filepath, "utf8"));
  } catch (err) {
    die(`Invalid JSON file in ${filepath}: ${err.message}`, "Verify file syntax.");
  }
}

function saveJsonFile(filepath, data) {
  const dir = path.dirname(filepath);
  if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
  const tempPath = `${filepath}.tmp.${Date.now()}`;
  fs.writeFileSync(tempPath, JSON.stringify(data, null, 2), "utf8");
  fs.renameSync(tempPath, filepath);
}

// ---------------------------------------------------------------------------
// Core Backfill Processor
// ---------------------------------------------------------------------------

function runBackfillForModule(mod, args) {
  const checkpointFile = getCheckpointPath(mod, args.checkpoint);
  const stateFile = getStatePath(mod, args.targetState);

  // Status check command
  if (args.status) {
    const cp = loadJsonFile(checkpointFile);
    if (!cp) {
      if (args.json) {
        console.log(JSON.stringify({ module: mod, status: "NotStarted", checkpoint: null }));
      } else {
        console.log(`Module '${mod}': No checkpoint exists (${checkpointFile}).`);
      }
      return { ok: true };
    }
    if (args.json) {
      console.log(JSON.stringify(cp, null, 2));
    } else {
      console.log(`\n=== Checkpoint Status: ${mod.toUpperCase()} ===`);
      console.log(`  Run ID:           ${cp.runId}`);
      console.log(`  Status:           ${cp.status}`);
      console.log(`  Cursor:           ${cp.cursor} / ${cp.totalRecords || 0}`);
      console.log(`  Created:          ${cp.stats.created}`);
      console.log(`  Updated:          ${cp.stats.updated}`);
      console.log(`  Skipped:          ${cp.stats.skipped}`);
      console.log(`  Failed:           ${cp.stats.failed}`);
      console.log(`  Last Updated:     ${new Date(cp.updatedAt).toISOString()}`);
      if (cp.lastError) {
        console.log(`  Last Error:       ${cp.lastError}`);
      }
      console.log(`  Rollback Entries: ${cp.rollbackJournal.length}`);
    }
    return { ok: true };
  }

  // Clear checkpoint command
  if (args.clearCheckpoint) {
    if (fs.existsSync(checkpointFile)) {
      fs.unlinkSync(checkpointFile);
      if (!args.json) console.log(`Cleared checkpoint for ${mod}: ${checkpointFile}`);
    } else {
      if (!args.json) console.log(`No checkpoint found for ${mod}`);
    }
    return { ok: true };
  }

  // Rollback command
  if (args.rollback) {
    const cp = loadJsonFile(checkpointFile);
    if (!cp || !Array.isArray(cp.rollbackJournal)) {
      die(`No valid rollback journal found in checkpoint ${checkpointFile}`, "Ensure a prior apply run created a checkpoint.");
    }
    const state = loadJsonFile(stateFile, {});
    let rolledBackCount = 0;

    for (let i = cp.rollbackJournal.length - 1; i >= 0; i--) {
      const entry = cp.rollbackJournal[i];
      if (entry.action === "CREATED") {
        delete state[entry.key];
        rolledBackCount++;
      } else if (entry.action === "UPDATED") {
        state[entry.key] = entry.previousValue;
        rolledBackCount++;
      }
    }

    saveJsonFile(stateFile, state);
    cp.status = "RolledBack";
    cp.updatedAt = Date.now();
    saveJsonFile(checkpointFile, cp);

    if (args.json) {
      console.log(JSON.stringify({ module: mod, status: "RolledBack", rolledBackRecords: rolledBackCount }));
    } else {
      console.log(`✓ Successfully rolled back ${rolledBackCount} mutation(s) for module '${mod}'. State restored.`);
    }
    return { ok: true };
  }

  // Load input data
  if (!args.input) {
    die("Missing required argument --input <file>", "Provide dataset path, e.g. --input fixtures/backfill/aid.json");
  }
  const inputRecords = loadJsonFile(path.resolve(args.input));
  if (!Array.isArray(inputRecords)) {
    die(`Input file ${args.input} must contain an array of records`, "Wrap entries in a top-level JSON array.");
  }

  if (inputRecords.length === 0) {
    die(`Input file ${args.input} contains 0 records`, "Provide at least 1 record to process.");
  }

  // Load target state
  const state = loadJsonFile(stateFile, {});

  // Checkpoint loading for resume
  let checkpoint = null;
  if (args.resume) {
    checkpoint = loadJsonFile(checkpointFile);
    if (!checkpoint) {
      die(`Cannot resume: checkpoint ${checkpointFile} does not exist`, "Run initial backfill before attempting resume.");
    }
    if (checkpoint.status === "Completed") {
      if (args.json) {
        console.log(JSON.stringify({ module: mod, status: "Completed", message: "Already completed." }));
      } else {
        console.log(`Module '${mod}': Backfill already completed in checkpoint. Nothing to resume.`);
      }
      return { ok: true };
    }
  }

  // -------------------------------------------------------------------------
  // DRY RUN DIFF PREVIEW
  // -------------------------------------------------------------------------
  if (args.dryRun) {
    const report = {
      module: mod,
      mode: args.mode,
      dryRun: true,
      totalScanned: inputRecords.length,
      toCreate: 0,
      toUpdate: 0,
      toSkip: 0,
      failedCount: 0,
      skippedRecords: [],
      failedRecords: [],
      diffs: [],
    };

    const seenBatchKeys = new Set();

    for (let i = 0; i < inputRecords.length; i++) {
      const record = inputRecords[i];
      const key = extractRecordKey(mod, record);

      if (!key) {
        report.failedCount++;
        report.failedRecords.push({
          index: i,
          key: `row_${i}`,
          error: "MissingRecordKey",
          reason: "Unable to extract primary record identifier",
          remediation: "Ensure record conforms to module schema with valid ID/keys.",
        });
        continue;
      }

      if (seenBatchKeys.has(key)) {
        if (args.duplicatePolicy === "reject") {
          report.failedCount++;
          report.failedRecords.push({
            index: i,
            key,
            error: "DuplicateRecordInBatch",
            reason: `Duplicate key '${key}' detected in input batch`,
            remediation: "Deduplicate input file or use --duplicate-policy skip/update",
          });
          continue;
        } else if (args.duplicatePolicy === "skip") {
          report.toSkip++;
          report.skippedRecords.push({ index: i, key, reason: "DuplicateInBatch" });
          continue;
        }
      }
      seenBatchKeys.add(key);

      const validation = validateRecord(mod, record);
      if (!validation.valid) {
        report.failedCount++;
        report.failedRecords.push({
          index: i,
          key,
          error: "ValidationError",
          reason: validation.reason,
          remediation: "Correct record attributes to satisfy module constraints.",
        });
        continue;
      }

      const existing = state[key];
      const diff = computeDiff(existing, record);

      if (diff.action === "SKIP") {
        report.toSkip++;
        report.skippedRecords.push({ index: i, key, reason: diff.reason });
      } else if (diff.action === "CREATE") {
        report.toCreate++;
        report.diffs.push({ index: i, key, action: "CREATE", changes: diff.changes });
      } else if (diff.action === "UPDATE") {
        report.toUpdate++;
        report.diffs.push({ index: i, key, action: "UPDATE", changes: diff.changes });
      }
    }

    if (args.json) {
      console.log(JSON.stringify(report, null, 2));
    } else {
      console.log(`\n============================================================`);
      console.log(`  DRY-RUN BACKFILL DIFF PREVIEW: [${mod.toUpperCase()}]`);
      console.log(`============================================================`);
      console.log(`Target state source: ${stateFile}`);
      console.log(`Total scanned:       ${report.totalScanned}`);
      console.log(`To create:           ${report.toCreate}`);
      console.log(`To update:           ${report.toUpdate}`);
      console.log(`To skip:             ${report.toSkip}`);
      console.log(`Validation failures: ${report.failedCount}`);
      console.log(`------------------------------------------------------------`);

      if (report.diffs.length > 0) {
        console.log(`\nProposed Mutations:`);
        for (const d of report.diffs) {
          if (d.action === "CREATE") {
            console.log(`  [CREATE] Key: ${d.key}`);
            for (const [k, v] of Object.entries(d.changes)) {
              console.log(`    + ${k}: ${JSON.stringify(v)}`);
            }
          } else if (d.action === "UPDATE") {
            console.log(`  [UPDATE] Key: ${d.key}`);
            for (const [k, c] of Object.entries(d.changes)) {
              console.log(formatDiffLine(k, c));
            }
          }
        }
      }

      if (report.skippedRecords.length > 0) {
        console.log(`\nSkipped Records (${report.skippedRecords.length}):`);
        for (const s of report.skippedRecords.slice(0, 10)) {
          console.log(`  • Key: ${s.key} (Reason: ${s.reason})`);
        }
        if (report.skippedRecords.length > 10) {
          console.log(`  ... and ${report.skippedRecords.length - 10} more`);
        }
      }

      if (report.failedRecords.length > 0) {
        console.log(`\nFailed Records (${report.failedRecords.length}):`);
        for (const f of report.failedRecords) {
          console.log(`  ✗ [FAIL] Key: ${f.key} (Error: ${f.error})`);
          console.log(`    Reason:      ${f.reason}`);
          console.log(`    Remediation: ${f.remediation}`);
        }
      }

      console.log(`\nNOTE: Dry-run mode completed. Zero persistent writes were made.`);
      console.log(`To commit these changes, re-run with '--apply'.\n`);
    }

    return { ok: report.failedCount === 0 || args.mode === "best-effort" };
  }

  // -------------------------------------------------------------------------
  // APPLY / RESUME EXECUTION WITH RESUMABLE CHECKPOINTS
  // -------------------------------------------------------------------------
  if (!checkpoint) {
    checkpoint = {
      runId: `run_${Date.now()}`,
      module: mod,
      status: "InProgress",
      mode: args.mode,
      cursor: 0,
      totalRecords: inputRecords.length,
      processedKeys: [],
      stats: {
        scanned: 0,
        created: 0,
        updated: 0,
        skipped: 0,
        failed: 0,
      },
      skippedRecords: [],
      failedRecords: [],
      rollbackJournal: [],
      startedAt: Date.now(),
      updatedAt: Date.now(),
      lastError: null,
    };
    saveJsonFile(checkpointFile, checkpoint);
  }

  const processedSet = new Set(checkpoint.processedKeys);
  const startIndex = checkpoint.cursor;
  const batchSize = Math.min(Math.max(args.batchSize, 1), MAX_BATCH_SIZE);

  if (!args.json) {
    console.log(`\nStarting backfill for module '${mod}'...`);
    console.log(`  Checkpoint:  ${checkpointFile}`);
    console.log(`  State File:  ${stateFile}`);
    console.log(`  Total:       ${inputRecords.length} records`);
    console.log(`  Resume from: index ${startIndex}`);
    console.log(`  Batch size:  ${batchSize}`);
  }

  let currentIndex = startIndex;
  let halted = false;

  while (currentIndex < inputRecords.length) {
    const batchEnd = Math.min(currentIndex + batchSize, inputRecords.length);
    const rollbackBatch = [];

    for (let i = currentIndex; i < batchEnd; i++) {
      const record = inputRecords[i];
      const key = extractRecordKey(mod, record);

      checkpoint.stats.scanned++;

      if (!key) {
        checkpoint.stats.failed++;
        const failObj = {
          index: i,
          key: `row_${i}`,
          error: "MissingRecordKey",
          reason: "Unable to extract primary record identifier",
        };
        checkpoint.failedRecords.push(failObj);
        if (args.mode === "all-or-nothing") {
          checkpoint.lastError = failObj.reason;
          halted = true;
          break;
        }
        continue;
      }

      if (processedSet.has(key)) {
        checkpoint.stats.skipped++;
        checkpoint.skippedRecords.push({ index: i, key, reason: "AlreadyProcessedInPriorBatch" });
        continue;
      }

      const validation = validateRecord(mod, record);
      if (!validation.valid) {
        checkpoint.stats.failed++;
        const failObj = {
          index: i,
          key,
          error: "ValidationError",
          reason: validation.reason,
        };
        checkpoint.failedRecords.push(failObj);

        if (args.mode === "all-or-nothing") {
          checkpoint.lastError = validation.reason;
          halted = true;
          break;
        }
        continue;
      }

      const existing = state[key];
      const diff = computeDiff(existing, record);

      if (diff.action === "SKIP") {
        checkpoint.stats.skipped++;
        checkpoint.skippedRecords.push({ index: i, key, reason: diff.reason });
      } else if (diff.action === "CREATE") {
        rollbackBatch.push({ action: "CREATED", key });
        state[key] = record;
        checkpoint.stats.created++;
      } else if (diff.action === "UPDATE") {
        rollbackBatch.push({ action: "UPDATED", key, previousValue: existing });
        state[key] = { ...existing, ...record };
        checkpoint.stats.updated++;
      }

      processedSet.add(key);
      checkpoint.processedKeys.push(key);
    }

    if (halted) {
      checkpoint.status = "AwaitingRecovery";
      checkpoint.cursor = currentIndex;
      checkpoint.updatedAt = Date.now();
      saveJsonFile(checkpointFile, checkpoint);
      break;
    }

    // Commit batch state and checkpoint
    checkpoint.rollbackJournal.push(...rollbackBatch);
    saveJsonFile(stateFile, state);

    currentIndex = batchEnd;
    checkpoint.cursor = currentIndex;
    checkpoint.updatedAt = Date.now();
    saveJsonFile(checkpointFile, checkpoint);
  }

  if (!halted && currentIndex >= inputRecords.length) {
    checkpoint.status = "Completed";
    checkpoint.updatedAt = Date.now();
    saveJsonFile(checkpointFile, checkpoint);
  }

  if (args.json) {
    console.log(JSON.stringify(checkpoint, null, 2));
  } else {
    console.log(`\n=== Backfill Summary: [${mod.toUpperCase()}] ===`);
    console.log(`  Status:      ${checkpoint.status}`);
    console.log(`  Processed:   ${checkpoint.cursor} / ${inputRecords.length}`);
    console.log(`  Created:     ${checkpoint.stats.created}`);
    console.log(`  Updated:     ${checkpoint.stats.updated}`);
    console.log(`  Skipped:     ${checkpoint.stats.skipped}`);
    console.log(`  Failed:      ${checkpoint.stats.failed}`);

    if (checkpoint.status === "AwaitingRecovery") {
      console.log(`\n[!] Operation halted due to failure under all-or-nothing mode.`);
      console.log(`    Last Error: ${checkpoint.lastError}`);
      console.log(`    Resume via: node scripts/backfill.cjs --module ${mod} --input ${args.input} --resume`);
    } else if (checkpoint.status === "Completed") {
      console.log(`\n✓ Backfill completed successfully.`);
    }
  }

  return { ok: checkpoint.status === "Completed" };
}

// ---------------------------------------------------------------------------
// Main Entrypoint
// ---------------------------------------------------------------------------

function main() {
  const args = parseArgs(process.argv.slice(2));

  if (!args.module) {
    die("Missing required argument --module <name>", "Specify one of: aid, treasury, referral, governance, oracle, marketplace, all.");
  }

  if (!SUPPORTED_MODULES.includes(args.module)) {
    die(`Unsupported module '${args.module}'`, `Must be one of: ${SUPPORTED_MODULES.join(", ")}`);
  }

  if (args.module === "all") {
    const modulesInOrder = ["governance", "treasury", "referral", "oracle", "aid", "marketplace"];
    let allOk = true;
    for (const m of modulesInOrder) {
      const moduleArgs = { ...args, module: m };
      // derive module-specific input if a directory or standard fixture
      if (args.input && fs.existsSync(args.input)) {
        const stat = fs.statSync(args.input);
        if (stat.isDirectory()) {
          moduleArgs.input = path.join(args.input, `${m}.json`);
        }
      }
      const res = runBackfillForModule(m, moduleArgs);
      if (!res.ok) allOk = false;
    }
    process.exit(allOk ? 0 : 1);
  } else {
    const res = runBackfillForModule(args.module, args);
    process.exit(res.ok ? 0 : 1);
  }
}

if (require.main === module) {
  main();
}

module.exports = {
  extractRecordKey,
  validateRecord,
  computeDiff,
  runBackfillForModule,
};
