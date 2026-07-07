#!/usr/bin/env bash
set -euo pipefail

target_triple="${TAURI_ENV_TARGET_TRIPLE:-}"
if [[ -z "$target_triple" ]]; then
  target_triple="$(rustc -vV | sed -n 's/^host: //p')"
fi

if [[ -z "$target_triple" ]]; then
  echo "Could not determine Rust target triple" >&2
  exit 1
fi

cargo_args=(build --release -p dopedb-mcp-stdio)
artifact_dir="target/release"
if [[ -n "${TAURI_ENV_TARGET_TRIPLE:-}" ]]; then
  cargo_args+=(--target "$target_triple")
  artifact_dir="target/$target_triple/release"
fi

bin_ext=""
if [[ "$target_triple" == *"windows"* ]]; then
  bin_ext=".exe"
fi

cargo "${cargo_args[@]}"
mkdir -p src-tauri/binaries
cp "$artifact_dir/dopedb-mcp-stdio$bin_ext" \
  "src-tauri/binaries/dopedb-mcp-stdio-$target_triple$bin_ext"
