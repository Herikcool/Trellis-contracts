#!/usr/bin/env bash
# Trellis config validation — fail fast on missing/malformed/unsafe config (Issue #66).
#
# Usage:
#   ./scripts/validate-config.sh            # validate current env, human output
#   ./scripts/validate-config.sh --quiet    # exit code only (for diagnostics.sh)
#   TRELLIS_ENV=local ./scripts/validate-config.sh
#
# Inputs (env vars):
#   TRELLIS_ENV        local|staging|production   (default: local)
#   STELLAR_NETWORK    standalone|local|futurenet|testnet|mainnet (or full passphrase)
#   STELLAR_RPC_URL    http(s)://... endpoint
#   TRELLIS_SECRET_KEY Stellar secret seed (S..., 56 chars) for non-local envs
#   TRELLIS_FEATURE_*  optional 0/1 flags (validated when present)
#
# Guarantees:
# - Exit non-zero with an actionable error on the first failure.
# - Secrets are NEVER printed in full (only first4...last2 preview).
set -u

QUIET=0
for arg in "$@"; do case "$arg" in --quiet) QUIET=1 ;; esac; done

log() { if [ "$QUIET" -eq 0 ]; then echo "$1"; fi; }
fail() { echo "CONFIG ERROR: $1" >&2; if [ "$QUIET" -eq 0 ] && [ -n "${2:-}" ]; then echo "  Fix: $2" >&2; fi; exit 1; }

ENV_VAL="${TRELLIS_ENV:-local}"
NETWORK="${STELLAR_NETWORK:-}"
RPC_URL="${STELLAR_RPC_URL:-}"
SECRET="${TRELLIS_SECRET_KEY:-}"

redact() { # redact <secret> -> first4...last2 or ***
  local s="$1"
  if [ "${#s}" -le 8 ]; then printf '***'; else printf '%s...%s' "${s:0:4}" "${s: -2}"; fi
}

case "$ENV_VAL" in local|staging|production) ;;
  *) fail "TRELLIS_ENV='$ENV_VAL' is unknown." "Set TRELLIS_ENV=local|staging|production." ;;
esac

[ -n "$NETWORK" ] || fail "STELLAR_NETWORK is missing." "Set STELLAR_NETWORK=testnet (dev), mainnet (prod), or standalone (local)."
case "$NETWORK" in
  standalone|local|futurenet|testnet|mainnet) ;;
  "Test SDF"*|"Public Global Stellar"*) ;;
  *) fail "STELLAR_NETWORK='$NETWORK' is malformed." "Use standalone|futurenet|testnet|mainnet or a full passphrase." ;;
esac

[ -n "$RPC_URL" ] || fail "STELLAR_RPC_URL is missing." "Set STELLAR_RPC_URL=https://soroban-testnet.stellar.org (testnet)."
case "$RPC_URL" in
  http://*|https://*) ;;
  *) fail "STELLAR_RPC_URL is malformed (must start with http:// or https://)." "Set a valid RPC endpoint URL." ;;
esac
if [[ "$RPC_URL" == *" "* ]]; then fail "STELLAR_RPC_URL contains whitespace." "Remove spaces from the URL."; fi

if [ "$ENV_VAL" = "production" ]; then
  case "$RPC_URL" in
    https://*) ;;
    *) fail "Production RPC must use https." "Set STELLAR_RPC_URL=https://... for production." ;;
  esac
fi

# Network & RPC consistency validation
case "$NETWORK" in
  mainnet|"Public Global Stellar"*)
    if [[ "$RPC_URL" == *"testnet"* ]] || [[ "$RPC_URL" == *"futurenet"* ]]; then
      fail "STELLAR_NETWORK is mainnet but STELLAR_RPC_URL points to a test network." "Use a mainnet RPC endpoint."
    fi
    if [[ "$RPC_URL" == *"localhost"* ]] || [[ "$RPC_URL" == *"127.0.0.1"* ]]; then
      fail "STELLAR_NETWORK is mainnet but STELLAR_RPC_URL points to localhost." "Use a public mainnet RPC endpoint."
    fi
    ;;
  testnet|futurenet|"Test SDF"*)
    if [ "$ENV_VAL" = "production" ]; then
      fail "Production environment cannot use test network '$NETWORK'." "Set STELLAR_NETWORK=mainnet."
    fi
    if [[ "$RPC_URL" == *"mainnet"* ]] || [[ "$RPC_URL" == *"public"* ]]; then
      fail "STELLAR_NETWORK is $NETWORK but STELLAR_RPC_URL points to mainnet." "Use a testnet RPC endpoint."
    fi
    if [[ "$RPC_URL" == *"localhost"* ]] || [[ "$RPC_URL" == *"127.0.0.1"* ]]; then
      fail "STELLAR_NETWORK is $NETWORK but STELLAR_RPC_URL points to localhost." "Use a valid remote testnet RPC."
    fi
    ;;
  standalone|local)
    if [ "$ENV_VAL" = "production" ] || [ "$ENV_VAL" = "staging" ]; then
      fail "$ENV_VAL environment cannot use standalone/local network." "Use mainnet (prod) or testnet (staging)."
    fi
    ;;
esac

# Module Contract IDs validation
MODULE_VARS=(
  "TRELLIS_TREASURY_ID"
  "TRELLIS_AID_ID"
  "TRELLIS_REFERRAL_ID"
  "TRELLIS_GOVERNANCE_ID"
  "TRELLIS_ORACLE_ID"
  "TRELLIS_MARKETPLACE_ID"
  "TRELLIS_REGISTRY_ID"
)

# Required contract IDs check
if [ "$ENV_VAL" = "production" ]; then
  for var in "${MODULE_VARS[@]}"; do
    val="${!var:-}"
    [ -n "$val" ] || fail "$var is missing in production." "Provide a deployed 56-char contract ID (C...)."
  done
elif [ "$ENV_VAL" = "staging" ]; then
  for var in "${MODULE_VARS[@]:0:6}"; do
    val="${!var:-}"
    [ -n "$val" ] || fail "$var is missing in staging." "Provide a deployed 56-char contract ID (C...)."
  done
fi

# Validate format and collect present IDs for distinctness check
declare -A SEEN_IDS
for var in "${MODULE_VARS[@]}"; do
  val="${!var:-}"
  if [ -n "$val" ]; then
    if [ "${#val}" -ne 56 ] || [[ "$val" != C* ]]; then
      fail "$var='$val' is malformed (expected C... 56 chars base32)." "Check the contract address."
    fi
    if [[ "$val" =~ [^A-Z2-7] ]]; then
      fail "$var='$val' contains invalid characters (must be base32 uppercase A-Z, 2-7)." "Provide a valid strkey contract ID."
    fi
    if [[ "$val" == *"PLACEHOLDER"* ]] || [[ "$val" == *"EXAMPLE"* ]]; then
      fail "$var='$val' is a placeholder." "Deploy the contract and set the real contract ID."
    fi
    # Check for cross-module collision
    if [ -n "${SEEN_IDS[$val]:-}" ]; then
      fail "Contract ID collision: $var and ${SEEN_IDS[$val]} share the same contract address '$val'." "Each module must have a distinct contract ID."
    fi
    SEEN_IDS["$val"]="$var"
  fi
done

# Secrets: required outside local; unsafe patterns always rejected.
case "$SECRET" in
  ""|"changeme"|"password"|"secret"|"test"|"testing"|"YOUR_SECRET_HERE"|"SECRETS_PLACEHOLDER"|"short"|"test-secret")
    if [ "$ENV_VAL" = "local" ] && [ -z "$SECRET" ]; then
      log "OK: local mode without a secret (ephemeral/test accounts only)."
    else
      fail "TRELLIS_SECRET_KEY value $(redact "$SECRET") is unsafe/placeholder." "Provide a real key via env/secret manager; never commit keys. Local dev must not reuse production keys."
    fi
    ;;
  *)
    if [ "${#SECRET}" -ne 56 ] || [[ "$SECRET" != S* ]]; then
      fail "TRELLIS_SECRET_KEY value $(redact "$SECRET") is malformed (expected S... 56 chars)." "Check for truncation/newlines; load from your secret manager."
    fi
    if [ "$ENV_VAL" = "local" ]; then
      fail "Production-like secret $(redact "$SECRET") must not be used with TRELLIS_ENV=local." "Unset TRELLIS_SECRET_KEY locally or use an ephemeral test key workflow."
    fi
    log "OK: secret $(redact "$SECRET") present with valid shape for $ENV_VAL."
    ;;
esac

# Optional feature flags: validate when present.
for var in $(compgen -e | grep '^TRELLIS_FEATURE_' || true); do
  val="${!var}"
  case "$val" in 0|1|true|false|TRUE|FALSE|True|False) ;;
    *) fail "$var='$val' is malformed." "Set $var=0|1 (or true|false)." ;;
  esac
done

log "OK: config valid (env=$ENV_VAL network=$NETWORK rpc=$RPC_URL)."
