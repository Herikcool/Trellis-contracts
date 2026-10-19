#!/usr/bin/env node
/**
 * Executable Verification Suite for Issue #227:
 * Stable cursor pagination under inserts, deletes, and permission changes.
 *
 * Verifies that:
 * 1. Keyset pagination remains strictly deterministic under concurrent mutations.
 * 2. Concurrently inserted records never cause shifts, duplicates, or missed items.
 * 3. Concurrently deleted records (including the cursor record itself) do not disrupt pagination.
 * 4. Dynamic permission revocations and visibility hides are evaluated at query time and NEVER leak forbidden records.
 * 5. Sparse clusters of forbidden or deleted records honor max_scan bounds and yield safe resumption cursors.
 * 6. Traversal is fully supported and deterministic in both Ascending and Descending directions.
 * 7. Boundary conditions (empty sets, boundaries, out-of-range cursors) behave predictably.
 */

"use strict";

const assert = require("node:assert");

console.log("======================================================================");
console.log("TRELLIS: STABLE CURSOR PAGINATION VERIFICATION RUNNER (#227)");
console.log("======================================================================\n");

// ---------------------------------------------------------------------------
// Pure JavaScript Reference Simulation of shared::pagination Keyset Engine
// ---------------------------------------------------------------------------

const Direction = {
  Ascending: "Ascending",
  Descending: "Descending",
};

const DEFAULT_PAGE_LIMIT = 20;
const MAX_PAGE_LIMIT = 100;
const DEFAULT_MAX_SCAN = 256;

class PageRequest {
  constructor(cursor, limit, direction = Direction.Ascending) {
    this.cursor = cursor ?? null;
    this.limit = limit === 0 ? DEFAULT_PAGE_LIMIT : Math.min(limit, MAX_PAGE_LIMIT);
    this.direction = direction;
  }

  static ascending(cursor, limit) {
    return new PageRequest(cursor, limit, Direction.Ascending);
  }

  static descending(cursor, limit) {
    return new PageRequest(cursor, limit, Direction.Descending);
  }
}

class PageResponse {
  constructor(items, nextCursor, hasMore, scannedCount) {
    this.items = items;
    this.next_cursor = nextCursor;
    this.has_more = hasMore;
    this.scanned_count = scannedCount;
  }
}

/**
 * Keyset pagination over a sorted array of IDs using binary search seeking.
 * Mirrors shared::paginate_id_list in Rust.
 */
function paginateIdList(ids, request, maxScan, fetchAndFilter) {
  const limit = request.limit === 0 ? DEFAULT_PAGE_LIMIT : Math.min(request.limit, MAX_PAGE_LIMIT);
  const totalLen = ids.length;
  const items = [];
  let scanned = 0;
  let lastProcessedId = null;
  let hasMore = false;

  if (totalLen === 0) {
    return new PageResponse(items, null, false, 0);
  }

  if (request.direction === Direction.Ascending) {
    let startIndex = 0;
    if (request.cursor !== null && request.cursor !== undefined) {
      const cursorId = BigInt(request.cursor);
      // Binary search for first index with id > cursorId
      let low = 0;
      let high = totalLen;
      while (low < high) {
        const mid = (low + high) >> 1;
        if (BigInt(ids[mid]) <= cursorId) {
          low = mid + 1;
        } else {
          high = mid;
        }
      }
      if (low >= totalLen) {
        return new PageResponse(items, null, false, 0);
      }
      startIndex = low;
    }

    let idx = startIndex;
    while (idx < totalLen && items.length < limit && scanned < maxScan) {
      const id = ids[idx];
      scanned += 1;
      lastProcessedId = id;

      const item = fetchAndFilter(id);
      if (item !== null && item !== undefined) {
        items.push(item);
      }
      idx += 1;
    }

    if (idx < totalLen) {
      hasMore = true;
    }
  } else {
    // Descending
    let startIndex = totalLen - 1;
    if (request.cursor !== null && request.cursor !== undefined) {
      const cursorId = BigInt(request.cursor);
      // Binary search for largest index with id < cursorId
      let low = 0;
      let high = totalLen;
      while (low < high) {
        const mid = (low + high) >> 1;
        if (BigInt(ids[mid]) < cursorId) {
          low = mid + 1;
        } else {
          high = mid;
        }
      }
      // low is first index where id >= cursorId
      if (low === 0) {
        return new PageResponse(items, null, false, 0);
      }
      startIndex = low - 1;
    }

    let idx = startIndex;
    let done = false;

    while (!done && items.length < limit && scanned < maxScan) {
      const id = ids[idx];
      scanned += 1;
      lastProcessedId = id;

      const item = fetchAndFilter(id);
      if (item !== null && item !== undefined) {
        items.push(item);
      }

      if (idx === 0) {
        done = true;
      } else {
        idx -= 1;
      }
    }

    if (!done) {
      hasMore = true;
    }
  }

  const nextCursor = hasMore ? lastProcessedId : null;
  return new PageResponse(items, nextCursor, hasMore, scanned);
}

// ---------------------------------------------------------------------------
// Test Suites
// ---------------------------------------------------------------------------

let passedCount = 0;
let totalCount = 0;

function runTest(name, fn) {
  totalCount++;
  try {
    fn();
    console.log(`  ✓ [PASS] ${name}`);
    passedCount++;
  } catch (err) {
    console.error(`  ✗ [FAIL] ${name}`);
    console.error(`    ${err.message}`);
    throw err;
  }
}

console.log(">>> Running Suite 1: Sequential Keyset Traversal (Ascending & Descending) ...");

runTest("Ascending multi-page sequential traversal", () => {
  const ids = [10, 20, 30, 40, 50, 60, 70];
  const req1 = PageRequest.ascending(null, 3);
  const p1 = paginateIdList(ids, req1, DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p1.items, [10, 20, 30]);
  assert.strictEqual(p1.next_cursor, 30);
  assert.strictEqual(p1.has_more, true);

  const req2 = PageRequest.ascending(p1.next_cursor, 3);
  const p2 = paginateIdList(ids, req2, DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p2.items, [40, 50, 60]);
  assert.strictEqual(p2.next_cursor, 60);
  assert.strictEqual(p2.has_more, true);

  const req3 = PageRequest.ascending(p2.next_cursor, 3);
  const p3 = paginateIdList(ids, req3, DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p3.items, [70]);
  assert.strictEqual(p3.next_cursor, null);
  assert.strictEqual(p3.has_more, false);
});

runTest("Descending multi-page sequential traversal", () => {
  const ids = [10, 20, 30, 40, 50, 60, 70];
  const req1 = PageRequest.descending(null, 3);
  const p1 = paginateIdList(ids, req1, DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p1.items, [70, 60, 50]);
  assert.strictEqual(p1.next_cursor, 50);
  assert.strictEqual(p1.has_more, true);

  const req2 = PageRequest.descending(p1.next_cursor, 3);
  const p2 = paginateIdList(ids, req2, DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p2.items, [40, 30, 20]);
  assert.strictEqual(p2.next_cursor, 20);
  assert.strictEqual(p2.has_more, true);

  const req3 = PageRequest.descending(p2.next_cursor, 3);
  const p3 = paginateIdList(ids, req3, DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p3.items, [10]);
  assert.strictEqual(p3.next_cursor, null);
  assert.strictEqual(p3.has_more, false);
});

console.log("\n>>> Running Suite 2: Determinism Under Concurrent Inserts ...");

runTest("Concurrent inserts before cursor do not duplicate seen records", () => {
  const ids = [10, 20, 30, 40, 50];
  // Page 1: reads 10, 20
  const p1 = paginateIdList(ids, PageRequest.ascending(null, 2), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p1.items, [10, 20]);
  assert.strictEqual(p1.next_cursor, 20);

  // Concurrently insert 5 (before seen), 15 (before cursor), 25 (future), 60 (end)
  const modifiedIds = [5, 10, 15, 20, 25, 30, 40, 50, 60];

  // Page 2: resumes strictly > 20
  const p2 = paginateIdList(modifiedIds, PageRequest.ascending(p1.next_cursor, 2), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p2.items, [25, 30]);
  assert.strictEqual(p2.next_cursor, 30);

  // Page 3: resumes strictly > 30
  const p3 = paginateIdList(modifiedIds, PageRequest.ascending(p2.next_cursor, 2), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p3.items, [40, 50]);
  assert.strictEqual(p3.next_cursor, 50);

  // Page 4: resumes strictly > 50 -> 60
  const p4 = paginateIdList(modifiedIds, PageRequest.ascending(p3.next_cursor, 2), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p4.items, [60]);
  assert.strictEqual(p4.next_cursor, null);
  assert.strictEqual(p4.has_more, false);
});

console.log("\n>>> Running Suite 3: Determinism Under Concurrent Deletes ...");

runTest("Cursor record itself deleted between requests resumes without stalling", () => {
  const ids = [10, 20, 30, 40, 50];
  const p1 = paginateIdList(ids, PageRequest.ascending(null, 2), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p1.items, [10, 20]);
  assert.strictEqual(p1.next_cursor, 20);

  // Deleting 10 (seen), 20 (cursor!), and 30 (unseen)
  const modifiedIds = [40, 50];

  const p2 = paginateIdList(modifiedIds, PageRequest.ascending(p1.next_cursor, 2), DEFAULT_MAX_SCAN, (id) => id);
  // Resumes strictly > 20, finding 40 and 50
  assert.deepStrictEqual(p2.items, [40, 50]);
  assert.strictEqual(p2.next_cursor, null);
  assert.strictEqual(p2.has_more, false);
});

console.log("\n>>> Running Suite 4: Dynamic Authorization & Permission Revocations ...");

runTest("Revoking viewer permission mid-traversal NEVER leaks forbidden records", () => {
  const ids = [100, 200, 300, 400, 500];
  const permissions = new Map([
    [100, true],
    [200, true],
    [300, true],
    [400, true],
    [500, true],
  ]);

  // Page 1: viewer reads 100, 200
  const p1 = paginateIdList(ids, PageRequest.ascending(null, 2), DEFAULT_MAX_SCAN, (id) => {
    return permissions.get(id) ? { id } : null;
  });
  assert.deepStrictEqual(p1.items.map((x) => x.id), [100, 200]);
  assert.strictEqual(p1.next_cursor, 200);

  // Before page 2: admin revokes permission on 300, and 400 is marked hidden
  permissions.set(300, false);
  permissions.set(400, false);

  // Page 2: seeking > 200 inspects 300 (denied), 400 (denied), finds 500 (allowed)
  const p2 = paginateIdList(ids, PageRequest.ascending(p1.next_cursor, 2), DEFAULT_MAX_SCAN, (id) => {
    return permissions.get(id) ? { id } : null;
  });
  assert.deepStrictEqual(p2.items.map((x) => x.id), [500]);
  assert.strictEqual(p2.next_cursor, null);
  assert.strictEqual(p2.has_more, false);

  // Confirm neither 300 nor 400 appeared in results
  const allReturnedIds = [...p1.items, ...p2.items].map((x) => x.id);
  assert(!allReturnedIds.includes(300), "Record 300 must not leak");
  assert(!allReturnedIds.includes(400), "Record 400 must not leak");
});

console.log("\n>>> Running Suite 5: Gas Budget & Sparse Cluster Protections (max_scan) ...");

runTest("Sparse forbidden clusters yield bounded partial pages and safe resume cursor", () => {
  const ids = [];
  for (let i = 1; i <= 20; i++) {
    ids.push(i);
  }

  // Only IDs 19 and 20 are authorized
  const isAuthorized = (id) => id >= 19;
  const maxScan = 5;

  // Page 1: scans 1..5, finds 0 items, yields next_cursor = 5
  const p1 = paginateIdList(ids, PageRequest.ascending(null, 5), maxScan, (id) => {
    return isAuthorized(id) ? id : null;
  });
  assert.strictEqual(p1.items.length, 0);
  assert.strictEqual(p1.scanned_count, 5);
  assert.strictEqual(p1.next_cursor, 5);
  assert.strictEqual(p1.has_more, true);

  // Page 2: scans 6..10, yields next_cursor = 10
  const p2 = paginateIdList(ids, PageRequest.ascending(p1.next_cursor, 5), maxScan, (id) => {
    return isAuthorized(id) ? id : null;
  });
  assert.strictEqual(p2.items.length, 0);
  assert.strictEqual(p2.scanned_count, 5);
  assert.strictEqual(p2.next_cursor, 10);
  assert.strictEqual(p2.has_more, true);

  // Page 3: scans 11..15, yields next_cursor = 15
  const p3 = paginateIdList(ids, PageRequest.ascending(p2.next_cursor, 5), maxScan, (id) => {
    return isAuthorized(id) ? id : null;
  });
  assert.strictEqual(p3.items.length, 0);
  assert.strictEqual(p3.next_cursor, 15);

  // Page 4: scans 16..20, finds 19 and 20!
  const p4 = paginateIdList(ids, PageRequest.ascending(p3.next_cursor, 5), maxScan, (id) => {
    return isAuthorized(id) ? id : null;
  });
  assert.deepStrictEqual(p4.items, [19, 20]);
  assert.strictEqual(p4.next_cursor, null);
  assert.strictEqual(p4.has_more, false);
});

console.log("\n>>> Running Suite 6: Boundary Conditions ...");

runTest("Empty collection returns empty response without error", () => {
  const p = paginateIdList([], PageRequest.ascending(null, 10), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p.items, []);
  assert.strictEqual(p.next_cursor, null);
  assert.strictEqual(p.has_more, false);
});

runTest("Cursor past maximum element returns empty response with has_more false", () => {
  const p = paginateIdList([10, 20, 30], PageRequest.ascending(999, 10), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p.items, []);
  assert.strictEqual(p.next_cursor, null);
  assert.strictEqual(p.has_more, false);
});

runTest("Descending cursor before minimum element returns empty response with has_more false", () => {
  const p = paginateIdList([10, 20, 30], PageRequest.descending(5, 10), DEFAULT_MAX_SCAN, (id) => id);
  assert.deepStrictEqual(p.items, []);
  assert.strictEqual(p.next_cursor, null);
  assert.strictEqual(p.has_more, false);
});

console.log("\n======================================================================");
console.log(`SUMMARY: ${passedCount} / ${totalCount} tests passed successfully.`);
console.log("======================================================================");

if (passedCount === totalCount) {
  console.log("\n✓ All stable cursor pagination criteria verified successfully!\n");
  process.exit(0);
} else {
  console.error("\n✗ One or more verification suites failed.\n");
  process.exit(1);
}
