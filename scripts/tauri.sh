#!/usr/bin/env bash
# Keep the normal `pnpm tauri ...` interface while routing macOS development
# through the stable-signing Cargo runner. Other platforms and commands pass through.
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAURI_BIN="$PROJECT_ROOT/node_modules/.bin/tauri"

if [ "$(uname -s)" = "Darwin" ] && [ "${1:-}" = "dev" ]; then
  shift
  exec "$TAURI_BIN" dev --runner "$PROJECT_ROOT/scripts/cargo-signed-runner.sh" "$@"
fi

exec "$TAURI_BIN" "$@"
