#!/usr/bin/env bash
# Cargo-compatible macOS dev runner. Tauri calls `runner run ...`; we build first,
# apply one stable code identity, then launch the exact binary Tauri would run.
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ "${1:-}" != "run" ]; then
  exec cargo "$@"
fi
shift

build_args=()
app_args=()
profile="debug"
target_triple=""
after_separator=false
previous=""

for arg in "$@"; do
  if [ "$after_separator" = true ]; then
    app_args+=("$arg")
    continue
  fi
  if [ "$arg" = "--" ]; then
    after_separator=true
    continue
  fi
  build_args+=("$arg")
  if [ "$arg" = "--release" ]; then
    profile="release"
  elif [ "$previous" = "--profile" ]; then
    profile="$arg"
  elif [ "$previous" = "--target" ]; then
    target_triple="$arg"
  fi
  previous="$arg"
done

cd "$PROJECT_ROOT"
cargo build "${build_args[@]}"

target_root="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
if [[ "$target_root" != /* ]]; then
  target_root="$PROJECT_ROOT/$target_root"
fi
if [ -n "$target_triple" ]; then
  binary="$target_root/$target_triple/$profile/dopedb"
else
  binary="$target_root/$profile/dopedb"
fi

bash "$PROJECT_ROOT/src-tauri/sign-dev.sh" "$binary"

if [ "${DOPEDB_SIGNED_RUNNER_CHECK_ONLY:-0}" = "1" ]; then
  exit 0
fi
if [ "${#app_args[@]}" -eq 0 ]; then
  exec "$binary"
fi
exec "$binary" "${app_args[@]}"
