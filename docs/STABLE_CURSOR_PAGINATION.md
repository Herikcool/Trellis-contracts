# Stable Keyset Cursor Pagination Under Mutations & Permission Changes

## 1. Overview & Context

In the Trellis humanitarian aid and DeFi smart-contract suite (covering treasury, aid, referral, governance, oracle, and marketplace modules), state changes continuously:
- **Inserts**: Donors create aid disbursements, governance proposals are submitted, treasury actions are scheduled, marketplace listings/auctions are opened.
- **Deletes / Terminal State Transitions**: Aids are settled or refunded, listings are cancelled, entries are archived.
- **Dynamic Permission Changes**: Donors restrict discovery, search access is granted or revoked, entries are redacted, or admin controls are updated.

Traditional offset-based pagination (`offset`, `limit` or vector array indices `cursor: u32`) fails under concurrent contract state mutations:
1. **Shifting & Skipping**: Concurrently deleting or settling an earlier record shifts all subsequent records left. A paginating client with an index cursor skips unvisited records.
2. **Duplicate Injection**: Concurrently inserting records shifts unvisited records right, causing clients to see duplicates of records already visited.
3. **Permission Leakage**: If access permissions change mid-traversal, relying on cached or pre-filtered index snapshots can leak newly forbidden or restricted records.
4. **Gas Exhaustion**: If hundreds of contiguous records are deleted or forbidden, a naive sequential scan loop consumes all available transaction gas and fails with a budget abort.

---

## 2. Architecture & Design Principles

Trellis implements deterministic keyset (cursor-based) pagination in `shared::pagination` and integrates it into Soroban contract modules (such as `contracts/aid-contract`):

```
                   Client Request (cursor = K, limit = L)
                                  │
                                  ▼
             Binary Search Seeking in Monotonic Index O(log N)
                   (Seeks strictly K_i > K or K_i < K)
                                  │
                                  ▼
           Bounded Loop (scanned < max_scan && items.len < limit)
                                  │
              ┌───────────────────┴───────────────────┐
              ▼                                       ▼
    Fetch Candidate Record                Exceeded max_scan Budget?
              │                                       │
              ▼                                       ▼
   Query-Time Authorization Filter           Yield Partial Page +
   (can_discover / role / permission)        next_cursor = last_scanned_id
              │
    ┌─────────┴─────────┐
    ▼                   ▼
 [Allowed]          [Forbidden]
Items.push()     Scanned++, Skip
```

### 2.1. Keyset Cursor Invariant
Each record possesses a unique, monotonic primary key $K \in \mathbb{N}$ (e.g. `u64` sequence ID).
- For **Ascending** order:
  $$\text{Query}(\text{cursor}) = \{ R_i \mid K_i > \text{cursor} \land \text{Authorized}(R_i, \text{viewer}) \}$$
- For **Descending** order:
  $$\text{Query}(\text{cursor}) = \{ R_i \mid K_i < \text{cursor} \land \text{Authorized}(R_i, \text{viewer}) \}$$

### 2.2. $O(\log N)$ Binary Search Seeking
Rather than scanning linearly from index `0` to reach the cursor position, `paginate_id_list` executes binary search over the sorted ID index to determine the starting offset in $O(\log N)$ steps. This protects contract execution against gas exhaustion even when index vectors grow.

### 2.3. Query-Time Authorization Filtering
Permissions and discovery rules are evaluated **dynamically at retrieval time** inside the `fetch_and_filter` predicate:
```rust
let res = shared::paginate_id_list(&env, &ids, &req, shared::DEFAULT_MAX_SCAN, |id| {
    if let Some(record) = get_aid(&env, id) {
        if can_discover(&env, &record, &viewer) {
            return Some(record);
        }
    }
    None
});
```
- If an aid was settled (`status != Pending`), hidden via `set_aid_search_visibility`, or if `revoke_search_access` was called, the filter evaluates to `None`.
- The candidate ID is counted against `scanned_count` and omitted from the response.
- Newly forbidden records can **never leak** to unauthorized callers.

### 2.4. Gas Budget & Sparse Cluster Protections (`max_scan`)
When a dataset contains dense clusters of settled, deleted, or forbidden records, the pagination engine limits the number of items evaluated in a single transaction by `max_scan` (default: 256).
- If `scanned == max_scan` before reaching `limit` items, the engine safely terminates the scan.
- It returns the items collected so far and sets `next_cursor = Some(last_inspected_id)`.
- The client resumes the query on the next transaction strictly after `last_inspected_id`, preventing out-of-gas aborts while guaranteeing forward progress.

---

## 3. Operational Edge Cases & Mitigation

| Edge Case | Scenario | Engine Behavior & Guarantee |
|:---|:---|:---|
| **Cursor Record Deleted** | The record corresponding to `cursor` was hard-deleted or pruned between requests. | Binary search seeks strictly $> \text{cursor}$ (or $< \text{cursor}$). Resumes at the next existing record without stalling or error. |
| **Concurrent Insert Before Cursor** | A record with $K < \text{cursor}$ is inserted between requests. | Skipped automatically because query filters strictly $> \text{cursor}$. Seen records are never duplicated. |
| **Concurrent Insert After Cursor** | A record with $K > \text{cursor}$ is inserted between requests. | Appears in proper sequence on subsequent pages. |
| **Mid-Traversal Revocation** | Viewer's permission on an upcoming record is revoked between pages. | Evaluated at query time; predicate returns `None`. Record is omitted; zero data leak. |
| **Entire Scan Window Forbidden** | An entire batch of $> \text{max\_scan}$ records is forbidden to viewer. | Returns empty `items: []`, with `has_more: true` and `next_cursor: Some(last_scanned)`. Client continues safely. |
| **Cursor Beyond Boundaries** | Cursor is greater than maximum key or less than minimum key. | Binary search detects boundary, returns `items: []`, `next_cursor: None`, `has_more: false`. |
| **Empty Collection** | Querying an empty table or filtered list. | Immediate $O(1)$ return with `items: []`, `next_cursor: None`. |

---

## 4. Failure Modes & Diagnostic Codes

| Diagnostic Code | Meaning | Remediation |
|:---|:---|:---|
| `ERR_PAG_LIMIT_EXCEEDED` | Caller passed `limit > MAX_PAGE_LIMIT` (100). | Client should cap request limit to `MAX_PAGE_LIMIT`. |
| `ERR_PAG_MAX_SCAN_EXHAUSTED` | Sparse cluster reached `max_scan` limit. | Normal behavior; client should continue querying using returned `next_cursor`. |
| `ERR_PAG_INDEX_UNSORTED` | Underlying index vector violated monotonic order. | Trigger index rebuild/repair via `repair_search_index`. |
| `ERR_PAG_STALE_RECORD` | Record ID in index has been pruned from storage. | Auto-handled: engine skips missing storage entries and continues scanning. |

---

## 5. Rollback & Recovery Steps

If a corrupted index or schema migration issue affects pagination:
1. **Index Repair**: Invoke `AidContract::repair_search_index` (or domain equivalent). The repair engine iterates monotonic storage keys and reconciles the index vector with active records.
2. **Safe Fallback**: Public APIs remain backward-compatible with legacy endpoints (`search_aids`, `list_aids_by_donor`).
3. **State Rollback**: In the event of an erroneous contract deployment, state is fully preserved because pagination is purely read-oriented and produces zero persistent state modifications.
