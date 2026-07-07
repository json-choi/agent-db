#!/usr/bin/env bash
# Sign the DEV binary with a STABLE code-signing identity.
#
# Why: `cargo build` output is *ad-hoc* signed, so its code hash changes on every
# rebuild. macOS records the Keychain "Always Allow" grant against the app's code
# identity — an ad-hoc identity is the per-build hash, so the grant never sticks and
# you get the "dopedb wants to use confidential information" prompt every launch.
# Signing with a stable identity (the same cert across rebuilds) makes the designated
# requirement stable, so "Always Allow" persists and the prompt stops.
#
# Prefers your existing "Apple Development" identity; falls back to a persistent
# self-signed cert if you have none. Runs automatically after `pnpm tauri:dev`/build
# via package.json, or by hand: bash src-tauri/sign-dev.sh [path-to-binary]
set -euo pipefail

BIN="${1:-target/debug/dopedb}"
ENTITLEMENTS="$(cd "$(dirname "$0")" && pwd)/entitlements.plist"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if [ ! -f "$BIN" ]; then
  echo "sign-dev: binary not found: $BIN (build first)" >&2
  exit 0   # don't fail the build; unsigned still runs (just re-prompts)
fi

# 1) Prefer a real Apple Development identity (trusted, stable, already present).
IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
  | grep -oE '"Apple Development: [^"]+"' | head -1 | tr -d '"')"

# 2) Otherwise use/create a persistent self-signed code-signing cert.
if [ -z "$IDENTITY" ]; then
  IDENTITY="dopedb-dev"
  if ! security find-certificate -c "$IDENTITY" "$KEYCHAIN" >/dev/null 2>&1; then
    echo "sign-dev: creating self-signed cert '$IDENTITY'…"
    OSSL="$(command -v /opt/homebrew/bin/openssl || command -v openssl)"
    tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
    PW="dopedbdev"   # non-empty: macOS security rejects empty-password PKCS12 MACs
    "$OSSL" req -x509 -newkey rsa:2048 -nodes -keyout "$tmp/k.pem" -out "$tmp/c.pem" \
      -days 3650 -subj "/CN=$IDENTITY" \
      -addext "extendedKeyUsage=codeSigning" -addext "basicConstraints=critical,CA:false" >/dev/null 2>&1
    # -legacy: OpenSSL 3 default PKCS12 is unreadable by macOS `security`.
    "$OSSL" pkcs12 -export -legacy -out "$tmp/id.p12" -inkey "$tmp/k.pem" -in "$tmp/c.pem" \
      -passout pass:"$PW" >/dev/null 2>&1
    # -A: let any tool use the key (dev-only) so codesign never prompts.
    security import "$tmp/id.p12" -k "$KEYCHAIN" -P "$PW" -A >/dev/null 2>&1
  fi
fi

codesign --force --sign "$IDENTITY" --entitlements "$ENTITLEMENTS" "$BIN"
echo "sign-dev: signed $BIN with [$IDENTITY]"
