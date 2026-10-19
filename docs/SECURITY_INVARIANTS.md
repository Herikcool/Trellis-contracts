# Security invariants

This document states the protocol-level invariants Trellis contracts rely on,
grounded in the actual entry points, storage keys, and error variants that
enforce them. It is a companion to [`AUDIT.id`](./AUDIT.md) (what gets
recorded when state changes) and [`THREAT_MODEL.md`](./THREAT_MODEL.md) (who
is trusted, and what an adversary can still do). None of the contracts listed
here have had a third-party audit — see [`../SECURITY.md`](../SECURITY.md).

Each invariant below is stated formally, names the function(s) that enforce
it, and names the error a violation actually returns today (not a proposed
one). Where the current implementation does not fully close a gap, that is
called out explicitly rather than glossed over — the point of this document
is to be checkable against the code, not to describe an idealized system.

---

## 1. Token conservation — Treasury, Aid, and escrow

### 1.1 Aid escrow

**Invariant.** For every `token`, the aid-contract's on-chain balance of
`token` equals the sum of `amount` over every `AidRecord` with
`status == AidStatus::Pending` and that `token`:

```
token.balance_of(aid_contract) == Σ { record.amount | record.status == Pending, record.token == token }
```

**Enforcement.** `AidContract::create_aid` (`contracts/aid-contract/src/lib.rs`)
writes the `AidRecord` with `status = Pending` *before* calling
`token::Client::transfer(&donor, &contract_address, &amount)` — the record and
the escrowed funds are created together. `AidContract::claim_aid` flips the
record to `Settled` and transfers `record.amount` from the contract to
`record.recipient` in the same call; `AidContract::refund_aid` flips it to
`Refunded` and transfers `record.amount` back to `record.donor`. Both
claim and refund are guarded to run at most once per record: `claim_aid`
rejects `Settled`/`Refunded` records with `AidError::AlreadyClaimed`, and
`refund_aid` separately rejects `Settled` with `AidError::AlreadyClaimed` and
`Refunded` with `AidError::AlreadyRefunded`. Because every status transition
(`Pending → Settled`, `Pending → Refunded`) is paired 1:1 with exactly one
token transfer of exactly `record.amount`, and a record can only transition
once, the sum above holds by construction. `AidError::NotExpiredYet` blocks a
refund from firing on the same funds a still-live claim window could also
settle.

### 1.2 Treasury category balances

**Invariant (as designed).** For every `(token, category)` pair, the stored
counter equals the funds earmarked for that category:

```
category_balance(token, category) == Σ deposits(token, category) − Σ withdrawals(token, category)
```

`TreasuryContract::category_balance(token, category)` reads the
`(BALANCE, token, category)k instance-storage entry (`contracts/treasury-contract/src/lib.rs`).
`deposit` increments it and immediately pulls `amount` of `token` from the
caller via `token::Client::transfer(&caller, &contract_address, &amount)`, so a deposit's bookkeeping and its token movement are atomic.

**Enforcement caveat — read this before relying on category balances for
reconciliation.** `withdraw`, `execute_scheduled_withdraw`,
`emergency_withdraw`, and `distribute_reward` all decrement the relevant
`(BALANCE, token, category)` counter and emit the corresponding withdrawal /
`CommissionPaid` event, but **none of them call `token::Client::transfer`** —
grep the file: the only `token::Client::new(...)` call in
`treasury-contract/src/lib.rs` is inside `deposit`. In the current
implementation, category balances are therefore a pure internal ledger of
*intended* outflows; they are not proof that `token` actually left the
contract on any withdrawal path. The practical consequence is:

```
token.balance_of(treasury_contract) >= Σ _categories category_balance(token, category)
```

is not guaranteed to hold as an equality the way it does for `deposit`/Aid
escrow — the real on-chain balance can only be *higher* than the sum of
booked categories (funds sent in via `deposit` or direct transfer that were
never booked out), never verified lower. Operators reconciling treasury state
should compare `category_balance` sums against the token contract's actual
`balance()` for the treasury address as a routine health check, and should
not assume a successful `withdraw`/`distribute_reward` call moved tokens
without independently verifying the recipient's balance. This is a
documentation-visible gap, not a hidden one: it is the single most
security-relevant fact in this file for anyone reasoning about "does
the treasury still hold what it says it holds."

**Guards that do hold regardless of the above:** every debiting path
validates `amount > 0` (`Error::InvalidArgument`), `amount <= withdrawal
limit` (`Error::WithdrawalLimitExceeded`, checked against `MAX_WD`), and
`amount <= category balance` (`Error::InsufficientBalance`) before mutating
storage, and `withdraw`/`execute_scheduled_withdraw` additionally consume a
quota via `shared::quota::check_and_consume`. `emergency_withdraw` further
requires `shared::storage::is_paused(&env)` to be `true` — it is unreachable
while the treasury is live — and only ever debits the `reserve` category.
`distribute_reward` requires the direct caller to be the address stored under
`REFERRAL_CONTRACT` (set via `set_referral_contract`, admin-only) with the
`ServiceActor` role, via `auth::require_permission(.., Permission::ServiceOperation)`;
an unset or mismatched caller gets `Error::Unauthorized`.

---

## 2. Referral graph acyclicity and non-dilution bounds

Source: `contracts/referral-contract/src/lib.rs`.

### 2.1 Acyclicity

**Invariant.** Following `Referrer(wallet) -> referrer` edges from any wallet
never revisits a wallet already on the path, for at least
``MAX_SUPPORTED_TIERS` (10) hops.

**Enforcement.** Both `set_referrer` (admin-only) and `register`
self-service, `wallet.require_auth()`) call `would_create_cycle(env,
referred_wallet, referrer)` before writing the `Referrer` edge. That function
walks upward from `referrer` through existing `Referrer` edges for up to
`MAX_SUPPORTED_TIERS` hops, returning `true` (rejected with
`Error::InvalidArgument`) if `referred_wallet` appears anywhere on that walk.
Self-referral (`wallet == referrer`) is rejected by the same error before the
walk even starts. `register` additionally requires the target `referrer` to
already have its own referrer edge (`read_referrer(&env, &referrer).is_none()`
→ `Error::InvalidArgument`), which prevents a caller from bootstrapping a
fresh, disconnected sub-graph outside admin control.

**Known bound.** `would_create_cycle`'s walk is capped at
``MAX_SUPPORTED_TIERS` hops (`depth < MAX_SUPPORTED_TIERS`); a cycle that
closes strictly beyond that depth would not be detected by this check alone.
This is bounded in practice by a second, independent invariant: `accrue`'s
own traversal (§2.2) is capped at `max_tiers` (validated ≤
``MAX_SUPPORTED_TIERS` by `set_tier_config`), so reward accrual can never walk
far enough to enter a cycle even if one existed past the checked depth. The
acyclicity check is defense-in-depth for graph *correctness* (e.g. off-chain
consumers walking the full chain); it is not load-bearing for fund safety,
which rests on the bounded accrual walk instead.

### 2.2 Tier commission non-dilution

**Invariant.** For a single `accrue(base_amount)` call, total commission
credited across all tiers never exceeds `base_amount`:

```
Σ_tier credited(tier) <= base_amount
```

**Enforcement.** `set_tier_config`'s validator, `validate_tier_config`,
requires every individual `tier_bps[i]` to be in `MIN_TIER_BPS..=MAX_TIER_BPS`
(`0..=10_000`, i.e. 0–100%) *and* sums all configured tiers' bps into
`total_bps`, rejecting the whole configuration with `Error::InvalidArgument`
if `total_bps > MAX_TIER_BPS` (10,000 = 100%). Since `accrue` computes each
tier's commission as `shared::math::bps_of(base_amount, tier_bps)`, and the
sum of all `tier_bps` values is capped at 10,000, the sum of all tiers'
commissions is capped at `base_amount` by construction — no configuration can
be accepted that would pay out more than the base amount across the whole
referral chain.

### 2.3 Lifetime reward cap

**Invariant.** For every referrer, cumulative credited commission never
exceeds the configured `reward_cap`:

```
lifetime_accrued(referrer) <= reward_cap  (for all time)
```

**Enforcement.** `credit_referrer` reads `lifetime_accrued(referrer)` before
crediting, short-circuits to `0` credited if it is already `>= reward_cap`,
and otherwise clamps `credited = min(commission, reward_cap -
lifetime_accrued)`. Both `accrued_balance` and `lifetime_accrued` are updated
with `shared::math::safe_add`, which returns `Error::Overflow` rather than
wrapping. `accrue` itself is admin/service-gated
(`require_admin` → `Permission::ReferralConfiguration`) — an end wallet
cannot self-trigger accrual, so Sybil-registering many wallets under oneself
cannot mint rewards without a privileged caller choosing to call `accrue`
against attacker-controlled traffic.

---

## 3. Oracle staleness and quorum bounds

Source: `contracts/oracle-contract/src/lib.rs`, `errors.rs`.

**What the contract actually enforces today:** replay protection, a future-skew
bound, a staleness window, decimals consistency, and a minimum submission
quorum. **It does not enforce a maximum deviation between a submitted price
and the current median** — there is no such check in `submit_price` or
`aggregate_feed_latest`. Stating this precisely (rather than inventing a
basis-point threshold that isn't in the code), is intentional; see
[``THREAT_MODEL.md`](./THREAT_MODEL.md) for the residual risk this leaves.

### 3.1 Submitter authorization

**Invariant.** Only addresses with an active submitter registration may post
prices. `OracleContract::submit_price` requires `submitter.require_auth()`
and rejects with `OracleError::SubmitterNotAuthorized` (500) unless `storage::is_submitter_active(&env, &submitter)` is `true`. Submitters are
added by `register_submitter` and removed by `deactivate_submitter`, both
gated by `shared::auth::require_admin`, which return
`OracleError::Unauthorized` (508) for a non-admin caller.

### 3.2 Replay protection

**Invariant.** Each `(submitter, feed_id)` pair's accepted nonce is strictly
increasing by exactly 1. `submit_price` reads
cstorage::get_nonce(&env, &submitter, &feed_id)` as `expected_nonce` and
rejects with `OracleError::DuplicateSubmission` (501) unless `nonce ==
expected_nonce + 1`; on success it writes the new nonce with
`storage::set_nonce`. A submission cannot be replayed, reordered, or skipped
without the submitter incrementing sequentially.

### 3.3 Staleness and future-skew bounds

**Invariant.** A submission's declared `timestamp` must satisfy:

```
current_time - staleness_window <= timestamp <= current_time + MAX_FUTURE_SKEW_SECS
```

where `MAX_FUTURE_SKEW_SECS = 60` (a fixed contract constant) and
`staleness_window` is admin-configurable via `set_staleness_window`
(`shared::auth::require_admin`-gated). `submit_price` rejects
ctimestamp > current_time.saturating_add(MAX_FUTURE_SKEW_SECS)` with
`OracleError::SubmissionFromFuture`, and rejects
`current_time > timestamp.checked_add(staleness_window)` (or an overflowing
add) with `OracleError::SubmissionStale` (504). The same window is
re-checked per-submission inside `aggregate_feed_latest` when building the
active set that feeds the median, so a submission that was fresh when
submitted but has since aged past `staleness_window` is silently excluded
ifrom the aggregate rather than corrupting it.

### 3.4 Quorum-gated aggregation

**Invariant.** `get_latest_price` never reflects fewer than
`MIN_PRICE_QUORUM` (2) independent, active, non-stale, decimals-consistent
submissions for that feed.

**Enforcement.** `aggregate_feed_latest` builds `prices` only from active
submissions whose submitter is still `is_submitter_active`, whose
`decimals` match the first-seen value for the batch (mismatched submissions
are excluded), and whose timestamp falls within the staleness window. If fewer
than `MIN_PRICE_QUODUM` submissions survive that filter, the call returns
`OracleError::InsufficientQuorum` (502) rather than a price derived from a
thin set. The median is computed over the surviving prices after sorting, so a
single outlier submission cannot move the reported value beyond the median
position of the active set.

---

## 4. Canonical serialization for signed, hashed, and replay-protected payloads

Source: `shared/src/canonical.rs` (canonical encoder), `shared/src/signature.rs`
(domain-separated hashing), and the consumers in `contracts/governance-contract`,
`contracts/marketplace-contract`, and `contracts/oracle-contract`.

**Invariant.** Every payload that is signed, committed to by hash, or checked
for replay is encoded by a single canonical encoder. Two payloads that are
semantically equal produce byte-identical encodings, and two payloads that
differ in any committed field produce different encodings:

```
canonical(a) == canonical(b)  <=>  a == b   (for the committed field set)
```

**Encoder rules (`shared::canonical`).** The encoder is deterministic and
depends only on the values of the fields, never on iteration order or on
host endianness:

1. **Stable field order.** Fields are encoded in the declaration order of the
   payload struct, and that order is part of the protocol contract. Reordering
   fields in the struct is a breaking change requiring a new domain tag and a
   migration note (see §4.4).
2. **Length-prefixed variable fields.** Every variable-length field
   (`Symbol`, `String`, `Bytes`, `Vec<T>`) is preficed with its length as a
   fixed-width `u32` big-endian integer, followed by the bytes. This makes
   concatenation unambiguous: `("ab", "c")` and `("a", "bc")` encode differently.
3. **Fixed-width integers in big-endian.** `u64`, `i128`, `u32`, and booleans
   are encoded big-endian at their native width; booleans are a single byte
   `0x00`/`0x01`. No host endianness leaks into the encoding.
4. **Normalized addresses.** `Address` values are encoded as their 32-byte
   Stellar strut representation (`Address::to_string` is not used), so two
   representations of the same account cannot diverge. Symbols are normalized to
   their declared form before encoding.
5. **Domain separation.** The canonical bytes are prefixed with a
   domain tag (`Trellis:`<domain>`:v1`) and the contract address before
   hashing, so a payload signed for one domain (e.g. governance proposal)
   cannot be replayed as another (e.g. marketplace listing).

**Enforcement call sites.** The following paths hash only through the
canonical encoder:

- Governance: `proposal_hash()` hashes the canonical encoding of the
  proposal body plus the proposer address and the creation ledger.
  Votes are recorded against that hash, and a vote whose recomputed hash
  differs from the stored one is rejected with `GovernanceError::HashMismatch`.
- Marketplace: listing and offer signatures are verified over the
  canonical encoding of the listing/offer struct; a mismatch returns
  `MarketplaceError::InvalidSignature`.
- Oracle: the submission digest used for nonce binding is computed over
  the canonical encoding of `(submitter, feed_id, nonce, price, timestamp,
  decimals)`, so a replay of any field changes the digest and fails the
  nonce check in £3.2.

**Regression tests.** `contracts/shared/src/canonical/tests.rs` asserts byte
equality for equivalent payloads and byte inequality for payloads that differ in
exactly one committed field. The cases covered include:

- two addresses with the same 32-byte strut but different `to_string()
  casing produce identical encodings;
- concatenation ambiguity cases (`("ab","c)` vs `("a","bc")`) produce different
  encodings;
- field reordering produces different encodings (guarding the stable
  order contract);
- domain tag changes produce different hashes for the same body.

**Operational edge cases and failure modes.**

- **Field order drift.** Any change to a payload struct's field order or type
  is a protocol-breaking change. Signatures and hashes produced before the
  change will fail verification after it. Rollback: revert the struct change
  and re-deploy; if the change is intended, bump the domain tag version and
  keep the old decoder available for in-flight payloads.
- **Signature verification failure.** A payload whose recomputed canonical hash
  does not match the stored hash is rejected before any state mutation. The
  caller should register the payload again from the original source; there
  is no partial application to unwind.
- **Nonce reuse.** A duplicate submission for the same `(submitter, feed_id)`
  fails with `OracleError::DuplicateSubmission`; the submitter must increment
  its nonce. There is no rollback path for a submitter that has already
  advanced its nonce and then lost the payload; the submitter must submit the
  next nonce with a fresh payload.
- Payloads that are semantically equal but differ in non-committed
  metadata (e.g. display labels) are encoded identically; only committed
  fields affect the hash.

**Rollback / recovery.** The canonical encoder is pure and stateless — a
rollback is a code revert and redeploy, not a storage migration. Payloads already
stored under the old encoder remain readable as long as the corresponding
 domain tag version is retained; the recommended recovery step is to redeploy
the previous encoder version and re-issue any signatures that failed under
the new one.

---

## 5. Cross-cutting notes

- The canonical encoder (§4) is the only sactioned way to produce a hash or
  signing input. Any code path that hashes a payload by any other means is
  considered a bug against this invariant.
- The category-balance reconciliation caveat in §1.2 is the most operationally
  relevant gap in this document; operators should treat treasury category
  balances as an internal ledger and verify token balances independently.
- Oracle aggregation enforces quorum and staleness but not a maximum deviation
  bound; see §3 and [`THREAT_MODEL.md`](./THREAT_MODEL.md).
- None of the contracts in this document have had a third-party audit.
