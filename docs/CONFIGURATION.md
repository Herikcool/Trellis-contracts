# Configuration & Secrets (Issue #66 & #219)

Startup and deployment fail fast on missing, malformed, inconsistent, or unsafe configuration across all supported environments (Local, Staging, Production). Secrets are never printed in full.

---

## 1. Environment Configuration Matrix

### 1.1 Core Runtime & Network Variables

| Var | Local | Staging | Production |
|---|---|---|---|
| `TRELLIS_ENV` | `local` | `staging` | `production` |
| `STELLAR_NETWORK` | `standalone` or `local` | `testnet` or `futurenet` | `mainnet` (or `Public Global Stellar...`) |
| `STELLAR_RPC_URL` | `http://localhost:8000` or `http://127.0.0.1:...` | `https://soroban-testnet.stellar.org` | `https://...` (strictly HTTPS, no localhost/testnet) |
| `TRELLIS_SECRET_KEY` | **absent** (ephemeral test keys only) | `S...` (56 chars, testnet funded) | `S...` (56 chars, loaded via Secret Manager) |
| `TRELLIS_FEATURE_*` | optional `0/1` or `true/false` | optional `0/1` or `true/false` | optional `0/1` or `true/false` |

### 1.2 Module Contract IDs

Every contract address must be a valid 56-character strkey beginning with `C` and composed of Base32 uppercase characters (`A-Z`, `2-7`). Non-deployed placeholders (`"PLACEHOLDER"`, `"EXAMPLE"`, or identical repeated characters) are rejected. In addition, **contract IDs across modules must be mutually distinct**; reusing a single contract ID for multiple modules fails configuration validation.

| Environment Variable | Module | Local | Staging | Production |
|---|---|---|---|---|
| `TRELLIS_TREASURY_ID` | Treasury Contract | Optional | **Required** | **Required** |
| `TRELLIS_AID_ID` | Humanitarian Aid Contract | Optional | **Required** | **Required** |
| `TRELLIS_REFERRAL_ID` | Referral Network Contract | Optional | **Required** | **Required** |
| `TRELLIS_GOVERNANCE_ID`| Governance Contract | Optional | **Required** | **Required** |
| `TRELLIS_ORACLE_ID` | Oracle Integration Contract | Optional | **Required** | **Required** |
| `TRELLIS_MARKETPLACE_ID`| NFT Marketplace Contract | Optional | **Required** | **Required** |
| `TRELLIS_REGISTRY_ID` | Protocol Registry Contract | Optional | Optional | **Required** |

### 1.3 Production-Critical Module Settings

| Module | Setting | Local Range | Staging Range | Production Requirement |
|---|---|---|---|---|
| **Treasury** | `max_withdrawal_limit` | `> 0` | `> 0` | `>= 1,000` and `<= 10^18` |
| **Aid** | `default_expiry_secs` | `60` .. `31,536,000` | `60` .. `31,536,000` | `>= 3,600` (min 1 hr) .. `31,536,000` (1 yr) |
| **Aid** | `treasury_id` / `token_id` | Valid format | Valid format | Must match deployed `TRELLIS_TREASURY_ID` |
| **Referral** | `max_tiers` & `tier_bps` | `1..10`, sum `<= 10,000` | `1..10`, sum `<= 10,000` | `1..10`, sum `<= 10,000` (100% cap) |
| **Referral** | `registry_id` | Optional | Optional | **Required**, must match `TRELLIS_REGISTRY_ID` |
| **Governance** | `threshold` | `>= 1` | `>= 1` | **`>= 2`** (multi-signature mandatory) |
| **Governance** | `admin_set` | `threshold <= N <= 20` | `threshold <= N <= 20` | `threshold <= N <= 20`, distinct accounts |
| **Oracle** | `min_price_quorum` | `>= 1` | `>= 1` | **`>= 2`** (independent submitter quorum) |
| **Oracle** | `max_future_skew_secs` | `10..300` | `10..300` | `10..300` (typically 60s) |
| **Marketplace** | `platform_fee_bps` | `0..1,000` (max 10%) | `0..1,000` | `0..1,000` with valid non-burned fee recipient |
| **Marketplace** | `bid_increment_bps` | `50..5,000` | `50..5,000` | `50..5,000` (0.5% to 50%) |
| **Marketplace** | `auto_extension_seconds` | `60..3,600` | `60..3,600` | `60..3,600` (1 min to 1 hr) |

---

## 2. Validation Rules and Guarantees

Mirrored between `shared::config` and `./scripts/validate-config.sh`:

1. **Fail-Fast Error Ordering**:
   - `Error::ConfigMissing`: Required environment variable or module contract ID is absent.
   - `Error::ConfigInvalid`: Malformed URL, network mismatch, out-of-range parameter, duplicate contract ID collision, or single-key governance in production.
   - `Error::UnsafeSecret`: Placeholder secret, short secret, or production-shaped secret seed accidentally used in local mode.
2. **Network & RPC Consistency**:
   - Mainnet network (`mainnet` or `Public Global Stellar...`) paired with a testnet, futurenet, localhost, or unencrypted HTTP endpoint fails immediately.
   - Testnet network paired with a mainnet or localhost endpoint fails immediately.
   - Standalone/local network used in Staging or Production fails immediately.
   - Production requires `https://` endpoint and non-empty valid seed.
3. **Cross-Module Contract ID Distinctness**:
   - Every module must maintain an independent deployed contract address. Collisions (e.g. `TRELLIS_AID_ID == TRELLIS_TREASURY_ID`) are caught before contract initialization.
4. **Secret Key Safety & Zero-Leakage Preview**:
   - Secrets are never printed in full. Loggers and error messages only surface `RedactedSecret` (`XXXX...YY` preview) or `***` for short values.
   - Production seeds used with `TRELLIS_ENV=local` fail fast to prevent accidental test transactions on production accounts.

---

## 3. Operational Edge Cases & Failure Modes

### 3.1 Edge Cases

- **Secret Rotation**:
  When rotating the deployment key `TRELLIS_SECRET_KEY`, the new key must be authorized in the Governance and Treasury contracts *before* switching the environment variable in runtime services. Rotating the key without updating on-chain permissions causes all administrative invocations to fail with `Unauthorized`.
- **RPC Failover & Network Latency**:
  During RPC failover, secondary RPC URLs must strictly adhere to the same network domain. An automated failover script must never fallback to an unencrypted HTTP endpoint or a testnet endpoint in production.
- **Cross-Contract Pointer Divergence (Split-Brain)**:
  If a contract is redeployed (e.g. Treasury upgraded to a new address) but dependent contracts (Aid, Referral) still point to the legacy address, operations like `claim_rewards` or `settle` will fail. `CrossEnvConfigBundle` and `validate-config.sh` enforce cross-module consistency across contract pointers.
- **Contract ID Collision / Copy-Paste Errors**:
  During staging or local deployment scripts, developers may accidentally copy the same contract address across multiple `.env` variables. Cross-module collision validation detects duplicates immediately.
- **Production Single-Admin Governance**:
  Configuring `threshold: 1` in production is rejected by `GovernanceSettings::validate` to prevent single-point-of-compromise vulnerabilities.

### 3.2 Failure Modes Matrix

| Failure Mode | Detected By | Error Code | Mitigation / Fix |
|---|---|---|---|
| Missing contract ID in prod | `ModuleContractIds::validate` | `ConfigMissing` | Deploy missing contract and set env var. |
| Duplicate contract ID across modules | `ModuleContractIds::validate` | `ConfigInvalid` | Ensure each module has a unique deployed address. |
| Mainnet pointing to testnet RPC | `validate_network_rpc_consistency` | `ConfigInvalid` | Update `STELLAR_RPC_URL` to match `STELLAR_NETWORK`. |
| Production using HTTP RPC | `validate_network_rpc_consistency` | `ConfigInvalid` | Configure HTTPS endpoint for production. |
| Production seed used in local | `validate_secret_key` | `UnsafeSecret` | Unset `TRELLIS_SECRET_KEY` locally; use ephemeral keys. |
| Weak or placeholder secret | `validate_secret_key` | `UnsafeSecret` | Fetch high-entropy seed from Secret Manager. |
| Governance threshold < 2 in prod | `GovernanceSettings::validate` | `ConfigInvalid` | Set threshold >= 2 with at least 2 distinct admin accounts. |
| Oracle quorum < 2 in prod | `OracleSettings::validate` | `ConfigInvalid` | Configure at least 2 independent submitters. |
| Aid expiry < 1 hour in prod | `AidSettings::validate` | `ConfigInvalid` | Set default expiry >= 3,600 seconds. |

---

## 4. Rollback and Recovery Procedures

### 4.1 Rollback Checklist for Misconfigured Deployments

1. **Halt Automated Invocations**:
   If an environment mismatch or parameter misconfiguration is detected post-deployment:
   ```bash
   # Execute verification script to pinpoint the failing variable
   ./scripts/validate-config.sh
   ```
2. **Circuit Breaker / Pause**:
   If invalid cross-contract addresses were initialized on-chain, pause affected contracts immediately using the authorized admin account:
   ```bash
   # Invokes pause() on contracts implementing emergency pause
   soroban contract invoke --id $TRELLIS_TREASURY_ID --source admin -- pause
   ```
3. **Revert Environment Configuration**:
   - Revert `.env` or Secret Manager parameters to the last known stable configuration.
   - Run `./scripts/validate-config.sh` to ensure all cross-environment constraints pass.
4. **State Reconciliation & Pointer Recovery**:
   - If Aid or Referral contracts were initialized with an incorrect Treasury address, update the pointer via admin governance:
     - For Referral: `set_treasury(caller, correct_treasury_address)`
     - For Referral Registry: `set_registry(caller, correct_registry_address)`
5. **Resume Protocol**:
   After verifying state consistency with `cargo test -p shared --test cross_env_config`:
   ```bash
   soroban contract invoke --id $TRELLIS_TREASURY_ID --source admin -- resume
   ```

---

## 5. Executable Verification & Automated Tests

Run the full validation suite:

```bash
# 1. Unit tests for config validators and edge cases:
cargo test -p shared --lib -- config::tests

# 2. Automated cross-environment contract tests:
cargo test -p shared --test cross_env_config

# 3. Off-chain deployment verification script:
./scripts/validate-config.sh

# 4. Quiet exit-code mode for CI pipelines:
./scripts/validate-config.sh --quiet
```

