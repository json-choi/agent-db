#!/usr/bin/env bash
# Compatibility entry point for older local scripts. Both required sidecars are
# staged so a legacy caller cannot accidentally build a package without the CLI.
set -euo pipefail

exec bash "$(cd "$(dirname "$0")" && pwd)/build-sidecars.sh"
