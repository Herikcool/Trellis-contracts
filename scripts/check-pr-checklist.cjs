#!/usr/bin/env node
/**
 * Validates a PR body against .github/PULL_REQUEST_TEMPLATE.md's checklist
 * (Issue #128 — release readiness). The template's own checklist already
 * says "Each item below is enforced by a job in ci.yml" — this is what
 * actually enforces the *checklist itself* rather than just the underlying
 * jobs, which catches the case where a contributor rewrote the template's
 * checklist section (or left every box unchecked) without CI otherwise
 * noticing anything wrong.
 *
 * Rules:
 *   - Every checkbox under "## Checklist" and "## On-chain Interface" must
 *     be checked (`- [x]`), UNLESS the body carries an exception marker.
 *   - "## Type of Change" only requires at least one checked box (it's a
 *     selection, not a checklist).
 *   - Exception marker (documented in docs/RELEASE_READINESS.md, for
 *     emergency fixes only):
 *       <!-- release-readiness-exception: <non-empty reason> -->
 *     If present with a non-empty reason, unchecked Checklist / On-chain
 *     Interface boxes are reported as an acknowledged exception instead of
 *     a failure — the reason is echoed into the job log so it's on record.
 *
 * Usage:
 *   node scripts/check-pr-checklist.cjs --body-file <file>
 *   node scripts/check-pr-checklist.cjs --body-file -        # read stdin
 *   node scripts/check-pr-checklist.cjs --template <file>    # default: .github/PULL_REQUEST_TEMPLATE.md
 *
 * Exit codes: 0 = pass (or acknowledged exception), 1 = incomplete checklist, 2 = could not run.
 */
"use strict";

const fs = require("fs");

const REQUIRE_ALL_SECTIONS = ["Checklist", "On-chain Interface", "Release Readiness Gate"];
const REQUIRE_ANY_SECTIONS = ["Type of Change"];
const EXCEPTION_RE = /<!--\s*release-readiness-exception:\s*(.+?)\s*-->/i;

function parseArgs(argv) {
  const out = { bodyFile: null, template: ".github/PULL_REQUEST_TEMPLATE.md" };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--body-file" && argv[i + 1]) out.bodyFile = argv[++i];
    else if (argv[i] === "--template" && argv[i + 1]) out.template = argv[++i];
  }
  return out;
}

function readInput(bodyFile) {
  if (bodyFile === "-") return fs.readFileSync(0, "utf8");
  return fs.readFileSync(bodyFile, "utf8");
}

/** Splits a markdown body into { heading: [lines] } by "## " headings. */
function sectionsOf(markdown) {
  const sections = {};
  let current = null;
  for (const line of markdown.split(/\r?\n/)) {
    const heading = line.match(/^##\s+(.+?)\s*$/);
    if (heading) {
      current = heading[1].trim();
      sections[current] = [];
      continue;
    }
    if (current) sections[current].push(line);
  }
  return sections;
}

/** Returns [{checked, text}] for every `- [ ]` / `- [x]` line. */
function checkboxesIn(lines) {
  const boxes = [];
  for (const line of lines) {
    const m = line.match(/^\s*-\s*\[([ xX])\]\s*(.*)$/);
    if (m) boxes.push({ checked: m[1].toLowerCase() === "x", text: m[2].trim() });
  }
  return boxes;
}

function main() {
  const { bodyFile, template } = parseArgs(process.argv.slice(2));
  if (!bodyFile) {
    console.error("✗ --body-file <file|-> is required");
    process.exit(2);
  }

  let body;
  let templateSource;
  try {
    body = readInput(bodyFile);
    templateSource = template && fs.existsSync(template) ? fs.readFileSync(template, "utf8") : null;
  } catch (err) {
    console.error(`✗ could not read input: ${err.message}`);
    process.exit(2);
  }

  const sections = sectionsOf(body);
  const templateSections = templateSource ? sectionsOf(templateSource) : {};

  const exceptionMatch = body.match(EXCEPTION_RE);
  const exceptionReason = exceptionMatch ? exceptionMatch[1].trim() : null;

  const problems = [];
  const exceptions = [];

  for (const name of REQUIRE_ALL_SECTIONS) {
    // A section the template defines but the PR body dropped entirely is
    // itself a problem — a contributor cannot satisfy the checklist by
    // deleting it.
    if (!(name in sections)) {
      if (name in templateSections || !templateSource) {
        problems.push(`section "## ${name}" is missing from the PR body`);
      }
      continue;
    }
    const boxes = checkboxesIn(sections[name]);
    if (boxes.length === 0) {
      problems.push(`section "## ${name}" has no checklist items to check`);
      continue;
    }
    const unchecked = boxes.filter((b) => !b.checked);
    if (unchecked.length > 0) {
      const detail = `${unchecked.length}/${boxes.length} unchecked in "## ${name}": ${unchecked
        .map((b) => `"${b.text}"`)
        .join(", ")}`;
      if (exceptionReason) exceptions.push(detail);
      else problems.push(detail);
    }
  }

  for (const name of REQUIRE_ANY_SECTIONS) {
    if (!(name in sections)) continue; // optional section; nothing to enforce
    const boxes = checkboxesIn(sections[name]);
    if (boxes.length > 0 && !boxes.some((b) => b.checked)) {
      problems.push(`section "## ${name}" must have at least one option checked`);
    }
  }

  if (exceptions.length > 0) {
    console.log(`⚠ Release-readiness exception acknowledged: "${exceptionReason}"`);
    for (const e of exceptions) console.log(`  - ${e}`);
    console.log("This bypass is on record in the job log per docs/RELEASE_READINESS.md.");
  }

  if (problems.length > 0) {
    console.error("✗ PR checklist is incomplete:\n");
    for (const p of problems) console.error(`  - ${p}`);
    console.error(
      "\nCheck every remaining box, or add an exception marker for an emergency fix " +
        "(see docs/RELEASE_READINESS.md):\n  <!-- release-readiness-exception: <reason> -->"
    );
    process.exit(1);
  }

  console.log("✓ PR checklist complete.");
  process.exit(0);
}

main();
