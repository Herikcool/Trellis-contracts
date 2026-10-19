#!/usr/bin/env bash
# Trellis operator-safe backfill runner wrapper (Issue #217).
#
# Delegates directly to scripts/backfill.cjs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NODE_BIN="${NODE_BIN:-node}"

if ! command -v "$NODE_BIN" >/dev/null 2>&1; then
  echo "BACKFILL ERROR: Node.js runtime not found on PATH." >&2
  exit 2
fi

exec "$NODE_BIN" "${SCRIPT_DIR}/backfill.cjs" "$@"
