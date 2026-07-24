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

cargo_args=(
  build
  --release
  --package dopedb-cli
  --package dopedb-mcp-stdio
)
target_root="${CARGO_TARGET_DIR:-target}"
if [[ "$target_root" != /* ]]; then
  target_root="$PWD/$target_root"
fi
artifact_dir="$target_root/release"
if [[ -n "${TAURI_ENV_TARGET_TRIPLE:-}" ]]; then
  cargo_args+=(--target "$target_triple")
  artifact_dir="$target_root/$target_triple/release"
fi

bin_ext=""
if [[ "$target_triple" == *"windows"* ]]; then
  bin_ext=".exe"
fi

cargo "${cargo_args[@]}"
mkdir -p src-tauri/binaries

stage_binary() {
  local source_path="$1"
  local destination_path="$2"
  if [[ ! -f "$source_path" ]]; then
    echo "Sidecar build artifact is missing: $source_path" >&2
    exit 1
  fi
  local temporary_path
  temporary_path="$(mktemp "src-tauri/binaries/.sidecar.XXXXXX")"
  trap 'rm -f "$temporary_path"' RETURN
  cp "$source_path" "$temporary_path"
  mv -f "$temporary_path" "$destination_path"
  trap - RETURN
}

stage_binary \
  "$artifact_dir/dopedb-mcp-stdio$bin_ext" \
  "src-tauri/binaries/dopedb-mcp-stdio-$target_triple$bin_ext"
stage_binary \
  "$artifact_dir/dopedb$bin_ext" \
  "src-tauri/binaries/dopedb-cli-$target_triple$bin_ext"
