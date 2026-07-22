#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
env_file="$repo_dir/.env.local"

if [[ ! -f "$env_file" ]]; then
  echo "Missing $env_file" >&2
  exit 1
fi

set -a
# shellcheck disable=SC1090
source "$env_file"
set +a

if [[ -z "${DATABASE_URL_UNPOOLED:-}" ]]; then
  echo "DATABASE_URL_UNPOOLED is required" >&2
  exit 1
fi

DATABASE_URL="$DATABASE_URL_UNPOOLED" pnpm --dir "$repo_dir/workspace-cloud" db:migrate
