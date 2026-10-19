# Contract-Level Asset Conservation Invariant Proofs & Formal Verification

/* Authorized Protocol Quality Assurance & Formal Verification Test Suite */

## 1. Executive Summary & Purpose

The **Trellis Contracts** suite manages financial value, humanitarian aid disbursements, escrow commitments, and protocol reserves on the Stellar/Soroban ledger. To provide mathematical assurance against unauthorized asset creation, loss, or misallocation, this framework implements **contract-level invariant proofs** that verify state conservation before and after every supported transition.

This document describes:
- The formal mathematical conservation model.
- The state transition matrix across financial contracts (`payments-contract`, `aid-contract`, `treasury-contract`).
- Invariant assertions on success, failure, and reversion paths.
- Negative testing via fixture mutations that prove tests fail when impossible balances occur.
- Assumptions, operational bounds, and known exclusions.

---

## 2. Formal Mathematical Conservation Model

### 2.1 The Closed Token System

For any token asset contract $\mathcal{T}$, let:
- $\mathcal{U} = \{ u_1, u_2, \dots, u_n \}$ be the set of all external participants (donors, beneficiaries, managers, fee recipients).
- $\mathcal{C}$ be the Trellis smart contract address.
- $B(x, t)$ denote the on-chain token balance of address $x$ at discrete ledger state $t$.
- $L(\mathcal{C}, t)$ denote the sum of all internal liability obligations recorded in contract storage at state $t$.

### 2.2 Global Conservation Invariant (Axiom I)

Across any state transition $\tau: t \to t+1$:
$$\sum_{u \in \mathcal{U}} B(u, t+1) + B(\mathcal{C}, t+1) = \sum_{u \in \mathcal{U}} B(u, t) + B(\mathcal{C}, t)$$

Equivalently, the net change in total system assets is strictly zero:
$$\Delta S = \Delta B_{\text{external}} + \Delta B_{\text{contract}} = 0$$

No tokens are spontaneously generated from thin air, destroyed, or lost to inaccessible storage keys.

### 2.3 Solvency & Backing Equivalence Invariant (Axiom II)

The contract's physical token balance held in the Stellar Asset Contract (SAC) must strictly match or exceed all internal accounting commitments:
$$B(\mathcal{C}, t) \ge L(\mathcal{C}, t)$$

Under strict conservation (where no untracked surplus is retained):
$$B(\mathcal{C}, t) = L(\mathcal{C}, t)$$

Where $L(\mathcal{C}, t)$ is defined per contract:
- **`payments-contract`**: $L = \text{total\_deposits} + \sum_{e \in \text{ActiveEscrows}} \text{amount}(e)$.
- **`aid-contract`**: $L = \sum_{a \in \text{PendingAids}} \text{amount}(a)$.
- **`treasury-contract`**: $L = \sum_{c \in \text{Categories}} \text{category\_balance}(c)$.

### 2.4 Reversion Invariance (Axiom III)

For any transition $\tau$ that encounters an invalid argument, unauthorized caller, expired deadline, or insufficient funds:
$$B(x, t+1) = B(x, t) \quad \forall x \in \mathcal{U} \cup \{ \mathcal{C} \}$$
$$L(\mathcal{C}, t+1) = L(\mathcal{C}, t)$$

Rejected transactions guarantee zero side effects and zero balance drift.

---

## 3. State Transition Matrix

| Contract | Entry Point | Path Type | External Delta ($\Delta B_{\text{ext}}$) | Contract Delta ($\Delta B_{\mathcal{C}}$) | Internal Liability Delta ($\Delta L_{\mathcal{C}}$) | Invariant Status |
|:---|:---|:---|:---:|:---:|:---:|:---:|
| **Payments** | `deposit` | Deposit | $-A$ (depositor) | $+A$ | $+A$ (`total_deposits`) | $\Delta S = 0$ |
| **Payments** | `withdraw` | Withdrawal | $+N$ (caller), $+F$ (fee recipient) | $-A$ ($A = N + F$) | $-A$ (`total_deposits`) | $\Delta S = 0$ |
| **Payments** | `withdraw_amount` | Partial WD | $+N$ (caller), $+F$ (fee recipient) | $-A$ ($A = N + F$) | $-A$ (`total_deposits`) | $\Delta S = 0$ |
| **Payments** | `create_escrow_entry` | Escrow | $-A$ (depositor) | $+A$ | $+A$ (`active_escrows`) | $\Delta S = 0$ |
| **Payments** | `release_escrow_entry` | Settlement | $+A$ (beneficiary) | $-A$ | $-A$ (`active_escrows`) | $\Delta S = 0$ |
| **Payments** | `refund_escrow_entry` | Cancellation | $+A$ (depositor) | $-A$ | $-A$ (`active_escrows`) | $\Delta S = 0$ |
| **Payments** | `batch_payout` | Batch Payout | $+\sum A_i$ (recipients) | $-\sum A_i$ | $-\sum A_i$ | $\Delta S = 0$ |
| **Aid** | `create_aid` | Disbursement | $-A$ (donor) | $+A$ | $+A$ (`pending_aids`) | $\Delta S = 0$ |
| **Aid** | `claim_aid` | Settlement | $+A$ (recipient) | $-A$ | $-A$ (`pending_aids`) | $\Delta S = 0$ |
| **Aid** | `refund_aid` | Cancellation | $+A$ (donor) | $-A$ | $-A$ (`pending_aids`) | $\Delta S = 0$ |
| **Treasury** | `deposit` | Category Funding | $-A$ (manager) | $+A$ | $+A$ (`category_balance`) | $\Delta S = 0$ |
| **Treasury** | `withdraw` | Withdrawal | $+A$ (recipient) | $-A$ | $-A$ (`category_balance`) | $\Delta S = 0$ |
| **Treasury** | `execute_scheduled_withdraw` | Scheduled WD | $+A$ (recipient) | $-A$ | $-A$ (`category_balance`) | $\Delta S = 0$ |
| **Treasury** | `distribute_reward` | Commission | $+A$ (recipient) | $-A$ | $-A$ (`rewards_balance`) | $\Delta S = 0$ |
| **Treasury** | `emergency_withdraw` | Emergency | $+A$ (to) | $-A$ | $-A$ (`reserve_balance`) | $\Delta S = 0$ |
| **All** | *Rejected Calls* | Failure | $0$ | $0$ | $0$ | $\Delta S = 0$ |

---

## 4. Fixture Mutation & Invariant Sensitivity Testing

To guarantee that the invariant test engine is sensitive and does not produce false negatives, explicit **mutation tests** inject corrupted state:

1. **Unbacked Reserve Inflation**: Simulates impossible asset creation where contract reserve increases without an external debit.
   - **Expected Error**: `InvariantViolation::AssetConservationViolated { net_delta > 0 }`.
2. **Unauthorized Reserve Drainage**: Simulates asset leakage where contract balance decreases without a legitimate withdrawal.
   - **Expected Error**: `InvariantViolation::AssetConservationViolated { net_delta < 0 }`.
3. **Internal Liability Desynchronization**: Simulates internal ledger corruption where liabilities exceed physical reserves.
   - **Expected Error**: `InvariantViolation::SolvencyBackingMismatched`.
4. **Negative Balance Corruption**: Simulates integer underflow or invalid balance states.
   - **Expected Error**: `InvariantViolation::ImpossibleBalanceDetected`.

---

## 5. Assumptions & Known Exclusions

### 5.1 Protocol Assumptions
- **Standard SAC Semantics**: The underlying token contracts implement standard Stellar Asset Contract semantics where transfer amounts are deterministic and 1:1.
- **Atomic Execution**: Soroban host environment guarantees atomic execution; cross-contract invocation failures revert all associated state changes.
- **Time/Sequence Monotonicity**: Expiry windows rely on monotonic ledger sequence numbers and timestamps (`env.ledger().sequence()`, `env.ledger().timestamp()`).

### 5.2 Known Exclusions
- **Rebasing & Fee-on-Transfer Tokens**: Tokens that algorithmically alter balances in holder accounts outside standard `transfer` calls (e.g., elastic supply rebasing tokens) violate the constant-sum assumption and are **not supported**.
- **Admin Minting / Burning Outside Contracts**: Token administrators minting directly to contract addresses or burning tokens via SAC administrator keys outside the contract's methods will alter reserves and require balance resynchronization.

---

## 6. Test Suite & Validation Commands

All invariant tests are automated and execute via `cargo test`:

```bash
# Run Payments invariant test suite
cargo test -p payments-contract invariants_test

# Run Aid invariant test suite
cargo test -p aid-contract invariants_test

# Run Treasury invariant test suite
cargo test -p treasury-contract invariants_test

# Run all invariant tests across the workspace
cargo test invariants_test
```
