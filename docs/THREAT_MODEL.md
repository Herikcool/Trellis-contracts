# Threat model

This document states who Trellis contracts trust, what an adversary can
still attempt against each contract family, and what to do when one of those
attempts succeeds. It is grounded in the entry points, roles, and error
variants that actually exist in `contracts/` today — not a generic Soroban
threat checklist. For the formal invariants these mitigations are built on,
see [`SECURITY_INVARIANTS.md`](./SECURITY_INVARIANTS.md). For disclosure
process and audit status, see [`../SECURITY.md`](../SECURITY.md): **these
contracts are unaudited and testnet-only.**

---

## 1. Trust assumptions

### 1.1 Admin keys

Two distinct admin models exist in this codebase, and they carry different
trust weight. Conflating them is itself a risk.

**access-control's multi-admin model** (`contracts/access-control/src/lib.rs`)
is an *any-admin* (logical OR) authority: `Admins` is a `Map<Address, bool>`,
and any address in it can call `add_admin`/`remove_admin` on any other
non-super admin (gated by the `ManageMaintainers` action, which only requires
maintainer authority — no second signer). A single compromised non-super
admin key is enough to add attacker-controlled admin accounts or remove
legitimate ones. The one address this model protects specially is the
super-admin: `remove_admin` hard-rejects `target == super_admin_addr` with
`AccessControlError::CannotRemoveSuperAdmin`, and the super-admin can only be
*replaced* through `transfer_ownership` → `accept_ownership`, a two-party,
expiring, opt-in handoff (see `SECURITY_INVARIANTS.md` §4.3). **If the
super-admin's own key is compromised, this protection is worthless** — the
attacker holds the very key the protocol trusts to authorize the transfer.

**governance-contract's admin model**
(`contracts/governance-contract/src/lib.rs`) is closer to true M-of-N: role
grants/revokes, parameter changes, and pause/unpause all go through
`propose` → `approve` → `execute`, where `execute` requires
`proposal.approval_count >= threshold` and each admin can `approve` a given
proposal at most once (`Error::AlreadyApproved`). A single compromised admin
key cannot execute a proposal alone if `threshold > 1`.

**The gap between them:** `set_admin_set` — the function that changes *who is
in* the admin set and *what the threshold is* — is gated only by
`require_admin_role` (single-caller `Permission::ManageRoles`), not by the
proposal/threshold flow it configures. A single compromised admin key with
that permission can call `set_admin_set` to shrink the admin set to itself
and drop the threshold to 1, then immediately `propose`/`approve`/`execute`
anything alone. This is a real, code-verifiable gap: `set_admin_set`
(`contracts/governance-contract/src/lib.rs`) does not route through
`propose`/`execute` the way every other privileged mutation in that contract
does.

**Mitigations available today:** rotate a suspected-compromised admin key
immediately via `remove_admin` (access-control) or `revoke_role` (governance)
from an uncompromised admin session; monitor
`ac_ad`/`ac_ar`/`ac_op`/`ac_oa` events (access-control admin/ownership
changes) and governance's `RoleGranted`/`RoleRevoked` events for
unattributed changes; treat any `set_admin_set` call as a high-severity
signal regardless of whether it was "authorized," since no second signer
reviewed it. See [`../SECURITY.md`](../SECURITY.md) for key-compromise
disclosure and `docs/RUNBOOK.md` §3.2 for live triage commands.

### 1.2 Oracle submitters

Oracle price feeds are trusted only to the extent of the
`register_submitter`/`deactivate_submitter` allowlist
(`contracts/oracle-contract/src/lib.rs`), both admin-gated. There is no
on-chain proof of the off-chain data source a submitter used — the contract
"anchors cryptographic proof that verification occurred" (per its own module
doc) but does not verify the *content* being anchored. Consensus is quorum +
median only: `MIN_PRICE_QUORUM = 2` distinct active, non-stale submitters are
sufficient to publish a `FeedLatest`, and there is **no maximum-deviation
check** between a new submission and the current median (see
`SECURITY_INVARIANTS.md` §3). This means the oracle's honesty assumption is
effectively "fewer than 2 of the registered submitters are malicious or
compromised at once," which is a much weaker bar than a typical N-of-M
oracle network with outlier rejection.

### 1.3 Stellar validators / network assumptions

Every time-based guard in this codebase — Aid's `expiry_ledger` check
(`env.ledger().sequence()`), Treasury's `TimeWindow` scheduling
(`env.ledger().timestamp()`), the oracle's staleness/future-skew window, and
access-control's invitation expiry/rate-limit windows — trusts Stellar's
consensus-derived ledger `sequence()`/`timestamp()` monotonicity and
liveness. If ledger close time could be manipulated or ledger closing halted,
staleness and expiry guards degrade accordingly (e.g. a halted ledger cannot
make a pending aid *become* refundable, but also cannot make an oracle
submission *become* stale). This is treated as out of scope for contract-level
mitigation — it is a base-layer assumption shared by every Soroban contract,
not something `Trellis-contracts` can harden against — and is called out here
only so it isn't silently assumed away.

---

## 2. Adversarial attack vectors and mitigations, by contract family

### 2.1 access-control

| Attack | Mitigation | Code |
|---|---|---|
| Escalate into a role you don't hold by inviting yourself | `InviteMember(role)` is role-scoped: a caller must already be a maintainer or hold `role` to invite into it | `require_action` → `AccessControlError::RoleEscalation` when a holder of a different role tries |
| Cancel someone else's invitation to grief or hijack it | `CancelInvitation(inviter)` is owner-scoped to the original inviter | same as above, proven by `inviter_may_cancel_only_their_own_invitation` |
| Spam invitations to burn storage / grief rate limits | Rolling 10-invites-per-hour cap per caller | `AccessControlError::RateLimitExceeded`, `create_invitation` |
| Introduce a cycle in the role hierarchy so two roles mutually imply each other | Self-reference and ancestor checks before the edge is written | `AccessControlError::SelfReference` / `CycleDetected`, `set_role_parent` |
| Remove the super-admin to strand the contract, or silently reassign it | Super-admin removal is hard-blocked; reassignment requires a two-party accept flow with expiry | `AccessControlError::CannotRemoveSuperAdmin`; `transfer_ownership`/`accept_ownership` |
| Replay/accept an ownership transfer after it should have lapsed | Expiry checked on accept, pending record cleared on expiry | `AccessControlError::TransferExpired` |
| A regular admin unilaterally adds more admins | Not currently mitigated — this is the any-admin (OR) model described in §1.1 | N/A — accepted design tradeoff, flagged here for operational awareness |

### 2.2 Treasury / Aid (fund-draining and accounting attacks)

| Attack | Mitigation | Code |
|---|---|---|
| Double-claim or double-refund an aid record to drain escrow twice | Status transitions are one-shot; both claim and refund check current status before mutating | `AidError::AlreadyClaimed` / `AidError::AlreadyRefunded` |
| Claim an aid after its expiry window | Expiry checked against `env.ledger().sequence()` before transfer | `AidError::Expired` |
| Refund a still-live (not yet expired) pending aid | Checked before status mutation | `AidError::NotExpiredYet` |
| Claim someone else's aid | `recipient` must match `record.recipient` and must `require_auth()` | `AidError::Unauthorized` |
| Withdraw more than the configured per-tx limit, or drain a category past its balance | Checked in cheap-first order before any mutation or auth commit | `Error::WithdrawalLimitExceeded`, `Error::InsufficientBalance` |
| Call `emergency_withdraw` during normal operation | Requires `shared::storage::is_paused(&env) == true`; unreachable while live | `Error::NotPaused` |
| Impersonate the referral contract to call `distribute_reward` | Caller must equal the address stored under `REFERRAL_CONTRACT` *and* hold `ServiceActor`/`ServiceOperation` | `Error::Unauthorized` |
| Reentrancy-style draining via a malicious token contract's callback during a transfer | Aid contract writes the new status (`Settled`/`Refunded`) to storage *before* invoking `token::Client::transfer` (checks-effects-interactions, documented in-line in `claim_aid`/`create_aid`) | `contracts/aid-contract/src/lib.rs` |
| Assume a successful `withdraw`/`emergency_withdraw`/`distribute_reward` call moved real tokens | **Not currently true** — see `SECURITY_INVARIANTS.md` §1.2. These paths decrement internal bookkeeping and emit events but do not call `token::Client::transfer`. Treat this as the highest-priority open finding in this document | `contracts/treasury-contract/src/lib.rs` |

The last row is the most consequential finding in this threat model: it is
not an attack an adversary needs to *execute* so much as a gap operators need
to *know about* — any integration that assumes `category_balance` decreasing
implies tokens moved should instead independently verify the recipient's
balance, or treasury bookkeeping and real holdings can silently diverge.

### 2.3 Referral (graph gaming / Sybil)

| Attack | Mitigation | Code |
|---|---|---|
| Self-referral to farm your own commission | Rejected before any graph mutation | `Error::InvalidArgument`, `register`/`set_referrer` |
| Register twice to attach to a second, more favorable referrer | Rejected if a `Referrer` edge already exists for the wallet | `Error::InvalidArgument`, `register` |
| Build a referral cycle so commission recurses back to the attacker | Bounded ancestor walk (`would_create_cycle`, up to `MAX_SUPPORTED_TIERS` hops) rejects the edge | `Error::InvalidArgument` |
| Sybil-register many wallets under yourself and self-trigger accrual to mint rewards | `accrue` is admin/service-gated (`Permission::ReferralConfiguration`); end wallets cannot call it themselves, so graph structure alone cannot mint value | `Error::Unauthorized` |
| Configure tier percentages that pay out more than the base transaction amount | Sum of all tier `bps` values capped at 10,000 (100%) at config time | `Error::InvalidArgument`, `validate_tier_config` |
| Extract unbounded commission over time from a single high-volume referral relationship | Per-referrer lifetime cap, clamped on every credit | `credit_referrer`, `reward_cap` |
| Double-claim accrued rewards | `claim_rewards` zeroes the accrued balance in the same call; a second claim reads `0` and returns early without touching treasury | `claim_rewards`, proven by `claim_rewards_distributes_from_treasury_and_double_claim_pays_nothing` |
| Drain the referral graph beyond the checked cycle depth | Bounded by `accrue`'s own `max_tiers` walk regardless (see `SECURITY_INVARIANTS.md` §2.1) — a cycle past the checked depth cannot be reached during accrual | `contracts/referral-contract/src/lib.rs` |

### 2.4 Oracle (price manipulation)

| Attack | Mitigation | Code |
|---|---|---|
| Post prices without being a registered submitter | Allowlist check before any state change | `OracleError::SubmitterNotAuthorized` |
| Replay a previously accepted submission | Strictly-incrementing per-`(submitter, feed_id)` nonce | `OracleError::DuplicateSubmission` |
| Backdate or pre-date a submission to game staleness windows | Future-skew (`MAX_FUTURE_SKEW_SECS = 60`) and staleness-window checks on submit and again at aggregation time | `OracleError::SubmissionFromFuture` / `SubmissionStale` |
| A single submitter moves the reported price alone | Aggregation requires `MIN_PRICE_QUORUM = 2`; median of 1 never publishes | `aggregate_feed_latest` returns `None` under quorum |
| **Two colluding/compromised submitters move the price arbitrarily far** | **Not mitigated on-chain today** — no deviation-from-prior-median check exists. Raising `MIN_PRICE_QUORUM`, adding a deviation band, or requiring an independent oracle is the recommended fix. Until then, treat any `FeedLatest` published by exactly `MIN_PRICE_QUORUM` submitters as low-confidence and cross-check against an off-chain reference before acting on it. | `contracts/oracle-contract/src/lib.rs` |

---

## 3. Cross-boundary trust decisions

A *cross-boundary trust decision* is any point where one contract family accepts an assertion produced by another contract family, an off-chain actor, or a different privilege domain, and acts on it without being able to re-derive the assertion itself. The tables above are organized by contract family; this section is organized by the *boundary* being crossed, because the highest-risk failures in this codebase occur at boundaries, not inside a single contract.

### 3.1 Boundary inventory

| Boundary | Assertion crossing it | Who can forge it | Consequence if forged |
|---|---|---|---|
| access-control → governance | "this caller holds `Permission::ManageRoles`" | a single compromised admin key | `set_admin_set` shrinks the admin set and threshold to 1 (§1.1) |
| governance → treasury | "this proposal was approved by `threshold` admins" | a single admin after `set_admin_set` | arbitrary treasury parameter changes |
| oracle → treasury/aid | "this price is the current median" | 2 colluding submitters | mispriced disbursements, no deviation band (§2.4) |
| referral → treasury | "this reward is owed to this referrer" | the referral contract itself, if its config is attacker-controlled | `distribute_reward` drains treasury bookkeeping |
| aid → token | "this transfer succeeded" | a malicious token contract | reentrancy during `claim_aid` (mitigated by CEI, §2.2) |
| treasury → token | "this withdrawal moved tokens" | **nobody — the call is not made** | bookkeeping diverges from real holdings (§2.2, last row) |
| off-chain submitter → oracle | "verification occurred off-chain" | any registered submitter | anchored proof of an unverified claim (§1.2) |

### 3.2 Abuse cases

**AC-1 — Single-key governance capture.** An attacker who compromises one admin key holding `ManageRoles` calls `set_admin_set` to reduce the admin set to `{attacker}` and `threshold` to `1`, then executes any proposal alone. *Detection:* a `set_admin_set` event with no preceding `propose`/`approve` pair. *Mitigation today:* rotate the key via `revoke_role` from an uncompromised session; treat the call as high-severity regardless of authorization. *Test:* `governance-contract` should assert that `set_admin_set` cannot be reached without `Permission::ManageRoles`, and that after a threshold reduction a single `approve` is insufficient when `threshold > 1` was the prior value.

**AC-2 — Quorum-minimum oracle push.** Two colluding submitters publish a price far from the prior median. *Detection:* compare each new `FeedLatest` against the previous median and flag deviations beyond an operational band (e.g. 5%). *Mitigation today:* none on-chain; operational cross-check required. *Test:* a unit test that submits two prices differing by >50% and asserts the contract *accepts* them — documenting the gap as a known, tested behavior rather than an untested assumption.

**AC-3 — Treasury bookkeeping divergence.** An operator observes `category_balance` decrease after `withdraw` and assumes tokens moved. *Detection:* reconcile on-chain `category_balance` against the treasury's actual token balance on a schedule. *Mitigation today:* none; the transfer call is absent. *Test:* an integration test asserting that after `withdraw`, the treasury's token balance is *unchanged* — pinning the current behavior so a future fix is a deliberate, reviewed change.

**AC-4 — Referral config self-dealing.** An admin with `ReferralConfiguration` sets tier `bps` summing to 10,000 and routes accrual to a self-controlled referrer. *Detection:* monitor `accrue` calls whose referrer is also the caller's admin. *Mitigation today:* the 10,000 bps cap and per-referrer lifetime cap bound the damage. *Test:* `validate_tier_config` rejects sums >10,000; `credit_referrer` clamps at `reward_cap`.

**AC-5 — Aid reentrancy via malicious token.** A token contract re-enters `claim_aid` during `transfer`. *Detection:* unexpected second `Settled` event for the same aid id. *Mitigation today:* status is written before the transfer (CEI). *Test:* a mock token that re-enters `claim_aid` must observe `AidError::AlreadyClaimed` on the second call.

### 3.3 Highest-risk paths and required checks

The following checks are the minimum executable verification for this threat model. Where the repository already has a test, the check is a pointer; where it does not, the check is the test to add.

1. **`set_admin_set` is not proposal-gated** — add a test asserting a single `ManageRoles` holder can call it directly, and document the result as the accepted risk in §1.1.
2. **Oracle accepts a >50% deviation** — add a test asserting two submitters can publish an arbitrary price; this is the executable form of AC-2.
3. **Treasury `withdraw` does not move tokens** — add a test asserting the treasury token balance is unchanged after `withdraw`; this is the executable form of AC-3.
4. **Aid CEI ordering** — assert a reentrant `claim_aid` fails with `AlreadyClaimed`.
5. **Referral double-claim** — already covered by `claim_rewards_distributes_from_treasury_and_double_claim_pays_nothing`.
6. **Access-control super-admin removal** — already covered by the `CannotRemoveSuperAdmin` path.

### 3.4 Operational edge cases and failure modes

| Edge case | Failure mode | Rollback / recovery |
|---|---|---|
| Super-admin key lost | No `transfer_ownership` can be initiated; contract is stranded | None on-chain — this is why the super-admin key must be a hardware-backed, backed-up key; see `docs/RUNBOOK.md` §3.2 |
| `set_admin_set` called unexpectedly | Admin set and threshold silently changed | Re-run `set_admin_set` from an uncompromised admin session to restore the prior set and threshold; rotate all keys that held `ManageRoles` |
| Oracle publishes a bad median | Downstream disbursements mispriced | Pause treasury (`shared::storage::set_paused`), deactivate the offending submitters via `deactivate_submitter`, re-publish from trusted submitters |
| Treasury bookkeeping diverges from holdings | `category_balance` no longer reflects real tokens | Halt withdrawals, reconcile off-chain, and correct bookkeeping via a reviewed admin transaction; do not assume `withdraw` moved funds |
| Aid escrow drained via reentrancy | Double settlement of one aid id | Status is already `Settled`; no rollback needed — the CEI ordering prevents the second transfer |
| Referral graph cycle past checked depth | Unbounded commission recursion | `accrue`'s own `max_tiers` walk bounds it; no rollback needed, but the edge should be removed via admin config |
| Ledger close halted | Expiry and staleness guards freeze | No contract-level action; resume when the network resumes. A halted ledger cannot make a pending aid refundable nor an oracle submission stale (§1.3) |

### 3.5 Rollback and recovery steps

1. **Detect** — monitor `ac_ad`/`ac_ar`/`ac_op`/`ac_oa`, governance `RoleGranted`/`RoleRevoked`, and any `set_admin_set` call. Treat unattributed changes as incidents.
2. **Contain** — pause treasury via `shared::storage::set_paused(true)`; deactivate suspect oracle submitters; revoke suspect roles from an uncompromised admin session.
3. **Eradicate** — rotate all keys that held the compromised permission; re-run `set_admin_set` to restore the intended admin set and threshold.
4. **Recover** — reconcile treasury bookkeeping against real token holdings; re-publish oracle feeds from trusted submitters; re-enable treasury by clearing the pause flag.
5. **Review** — record the incident against the relevant abuse case (AC-1…AC-5) and update this document if a new boundary was crossed.

---

## 4. Compatibility

All mitigations and checks in this document are additive: they add tests, documentation, and operational guidance. No public API, storage schema, event name, or user-facing behavior is changed. Any future fix that *does* change behavior (e.g. adding a deviation band to the oracle, or making `withdraw` actually transfer tokens) must ship with a migration plan and a versioned contract upgrade, and must update this document and `SECURITY_INVARIANTS.md` together.quiring more independent submitters per feed are the available design responses, none of which are implemented yet | `contracts/oracle-contract/src/lib.rs` |
| Keep an oracle key active after it should be revoked (departed integration, suspected compromise) | Admin-only deactivation, immediately excludes the submitter from future quorum and from the *active-submissions* set used in aggregation | `deactivate_submitter`, checked live in `aggregate_feed_latest` via `is_submitter_active` |

### 2.5 Governance (takeover)

| Attack | Mitigation | Code |
|---|---|---|
| A single admin executes a sensitive action (role grant, parameter change, pause) alone | `execute` requires `approval_count >= threshold`; `approve` is one-shot per admin per proposal | `Error::BelowThreshold`, `Error::AlreadyApproved` |
| Re-execute an already-executed or cancelled/expired proposal | Status checked before action dispatch | `Error::AlreadyExecuted`, `Error::ProposalCancelled` (checked via `ProposalStatus`) |
| Let a stale proposal linger and execute long after context changed | Proposals expire (`PROPOSAL_LIFETIME`); an expired proposal transitions to `Expired` on the next `approve`/`execute` touch and is no longer executable | `contracts/governance-contract/src/lib.rs` |
| Set an out-of-bounds parameter (e.g. a referral tier above 100%, a zero-length admin set) | Bounds validated before write | `Error::InvalidArgument`, `set_param`/`set_admin_set` |
| **Bypass the M-of-N flow entirely by reconfiguring who counts as "M" and "N"** | **Not mitigated** — `set_admin_set` is single-admin-gated, not proposal-gated (see §1.1). This is the top governance-family finding in this document | `contracts/governance-contract/src/lib.rs`, `set_admin_set` |

---

## 3. Emergency response / pause runbooks

This section is a pointer, not a duplicate: the authoritative incident
procedure — triage table, decision points, rollback steps, and the
communication template — is [`RUNBOOK.md`](./RUNBOOK.md). What follows is
specifically the *pause/circuit-breaker surface* across contracts, so an
on-call engineer knows which pause mechanism applies to which contract before
jumping to `RUNBOOK.md` §3.1/§3.6.

### 3.1 Three independent pause mechanisms exist — know which one you're touching

Trellis does **not** have one global pause switch. Each contract's pause
state is its own instance storage; pausing one contract has no effect on
another.

**access-control — scoped circuit breaker.** `pause(caller, scope)` /
`resume(caller, scope)`, gated by the `ManageMaintainers` action (maintainer
authority). Valid scopes are `PAUSE_SCOPE_ROLES` (`"roles"`),
`PAUSE_SCOPE_ADMINS` (`"admins"`), and `PAUSE_SCOPE_INVITES` (`"invites"`);
an unrecognized scope is rejected with `AccessControlError::InvalidPauseScope`
before any state changes. Pausing an already-paused scope returns
`AccessControlError::OperationPaused`; resuming a scope that isn't paused
returns `AccessControlError::NotPaused`. Each pause records who paused it and
when (`PausedBy`/`PausedAt`), and both pause and resume write an audit entry
via `record_access_audit` — check `is_paused(scope)` and `audit_trail` first
when triaging an access-control incident, since the *scope* determines what's
actually frozen (e.g. pausing `"invites"` does not stop `grant_role`).

**aid-contract and treasury-contract — shared boolean flag.**
`shared::storage::is_paused`/`set_paused` is a single boolean per contract
instance (not shared across contracts — each contract's storage is its own).
`AidContract::set_paused(admin, paused)` requires the `PauseContracts`
permission (delegatable via `set_pauser`, which grants the `Pauser` role);
while paused, `claim_aid` returns `AidError::Paused`. Note `create_aid` and
`refund_aid` are **not** gated by this flag in the current code — pausing
stops new claims, not new escrow creation or refunds of already-expired aid.
`treasury-contract` does not expose a `set_paused` entry point of its own —
the only reference to the paused flag in `contracts/treasury-contract/src/lib.rs`
is the read inside `emergency_withdraw`. Since each contract's instance
storage is private to that contract's own code, and `shared::storage::is_paused`
defaults to `false` until something calls `set_paused(true)` on that same
instance, **`emergency_withdraw` currently has no reachable path to `true`
through any entry point the treasury contract exposes** — it is effectively
dead code today unless a future change wires a `set_paused`/`set_pauser`
entry point into this contract the way `aid-contract` has one. Do not rely on
"pause the treasury" as an available incident response until that gap is
closed; verify with `cargo test -p treasury-contract` and a direct read of
`is_paused()` (if exposed) before assuming an emergency pause is possible.

**governance-contract — multisig-gated pause.** `ProposalAction::Pause` /
`Unpause` go through the full `propose`/`approve`/`execute` flow (§2.5); a
single admin cannot pause or unpause unilaterally when `threshold > 1`.
`GovernanceContract::is_paused()` reads the same `shared::storage::is_paused`
primitive as aid-contract, but only governance's own execute path is wired to
write it via a proposal.

### 3.2 Runbook pointer

For the actual incident-response steps — pause decision tree, rollback to a
known-good WASM, communication template, and the quota/import/escrow
triage procedures — follow [`RUNBOOK.md`](./RUNBOOK.md) directly; do not
duplicate it here. The one addition this document makes to that runbook: when
triaging a pause-related incident (`RUNBOOK.md` §3.1), first identify *which*
of the three mechanisms above is in play, since the getter to check
(`is_paused()` vs `is_paused(scope)`) and the admin action to unpause
(`resume(scope)` vs `set_paused(admin, false)` vs a governance `Unpause`
proposal) differ by contract.

### 3.3 Key-compromise response

If the compromised key is a super-admin or governance admin key, do not stop
at revoking it — also audit whether it was used to call `set_admin_set`
(governance) or `add_admin` (access-control) before revocation, since either
call could have planted a second, attacker-controlled admin that a simple key
rotation would miss. Follow the disclosure process in
[`../SECURITY.md`](../SECURITY.md) in parallel with any on-chain response.

---

## Summary of open findings referenced above

These are called out inline throughout this document; collected here for
visibility. None require a code change to *document* — they are the reason
this document exists — but each is a candidate follow-up issue:

1. Treasury `withdraw` / `execute_scheduled_withdraw` / `emergency_withdraw` /
   `distribute_reward` never call `token::Client::transfer` — category
   balances can diverge from real token holdings (§2.2, `SECURITY_INVARIANTS.md` §1.2).
2. Oracle aggregation has no maximum-deviation-from-median check; two
   colluding submitters (the minimum quorum) can move a feed arbitrarily
   (§2.4, `SECURITY_INVARIANTS.md` §3).
3. `governance-contract::set_admin_set` is single-admin-gated rather than
   proposal-gated, so it can be used to unilaterally lower the threshold that
   is supposed to protect every other governance action (§1.1, §2.5).
4. `access-control`'s admin model is any-admin (OR), not threshold — any one
   admin can create or remove other non-super admins (§1.1, §2.1).
5. `treasury-contract` exposes no `set_paused`/`is_paused` entry point of its
   own, so `emergency_withdraw`'s pause precondition has no reachable path to
   `true` today — the emergency reserve-withdrawal path is effectively
   unreachable (§3.1).
