# Building agent-db

macOS-native, agent-driven database client. Rust core (Tauri v2) + React/TS frontend.
Backend agent is **codex-only**, authenticated via the user's ChatGPT subscription.

## Prerequisites

- **Rust** (stable, ≥ 1.82) — `rustup` toolchain. The build pulls sqlx (pg/mysql/sqlite,
  rustls-ring TLS), Tauri v2, keyring, etc.
- **Node** ≥ 18 and **pnpm** (`corepack enable pnpm`, or `npm i -g pnpm`).
- **Xcode Command Line Tools** (`xcode-select --install`) for the macOS toolchain.
- **codex CLI**, installed and **logged in with a ChatGPT subscription**:
  ```sh
  codex login          # authenticates via ChatGPT OAuth, stored under ~/.codex
  codex login status   # should report: Logged in using ChatGPT
  ```
  agent-db spawns `codex` with a **scrubbed environment** (only HOME/PATH/TERM) so the
  child can read `~/.codex` but cannot see any DB credentials. There is **no API-key
  path** — subscription/OAuth only.

## Run in development

```sh
pnpm install
pnpm tauri dev
```

`tauri dev` runs Vite (frontend) and `cargo` (Rust core) together with hot reload.

To iterate on just one layer:

```sh
pnpm dev                                      # frontend only (Vite, port 1420)
cargo check --manifest-path src-tauri/Cargo.toml
pnpm exec tsc --noEmit                         # TS typecheck
```

## MCP stdio bridge (sidecar)

Claude Desktop / Codex reach the in-app MCP server through a tiny stdio↔TCP bridge
binary (`agent-db-mcp-stdio`, a separate workspace member). It ships as a Tauri
**sidecar** (`bundle.externalBin`), so it must be built and staged before packaging:

```sh
pnpm build:bridge   # cargo-builds the bridge, copies it to
                    # src-tauri/binaries/agent-db-mcp-stdio-<target-triple>
```

`build:bridge` is wired into **both** `beforeDevCommand` and `beforeBuildCommand`, so
`pnpm tauri dev` and `pnpm tauri build` stage it automatically — in dev the same binary
sits next to the debug app binary in `target/debug/`, which is where the one-click
Claude Desktop/Codex configs point. The triple comes from `rustc -vV` (host); a
cross-compiled build needs the matching `-<triple>` file present in `src-tauri/binaries/`.
The bridge is built with the `dev` profile (it only pumps bytes; no release tuning needed).

## Build a distributable (.dmg)

```sh
pnpm tauri build
```

Output lands in `src-tauri/target/release/bundle/`:
- `dmg/agent-db_<version>_aarch64.dmg` — the installer image
- `macos/agent-db.app` — the app bundle

For a **signed + notarized** build (required for the Keychain to work — see below), set
the standard Tauri signing env vars before `tauri build`:
`APPLE_CERTIFICATE`, `APPLE_SIGNING_IDENTITY` (a Developer ID Application cert),
`APPLE_ID`, `APPLE_PASSWORD`/`APPLE_API_KEY` for notarization. The app ships **off the
Mac App Store** (Developer ID, hardened runtime, no App Sandbox) — the sandbox forbids
spawning the external `codex` binary, so MAS distribution is structurally incompatible.

## Known limitations / deferred items

- **Keychain needs a signed build.** Unsigned/ad-hoc dev builds hit
  `errSecMissingEntitlement (-34018)`. In **debug builds only** we fall back to an
  obfuscated file under the app data dir (`dev-secrets/`). That fallback is NOT real
  security — it exists solely so unsigned dev builds run. Sign the build for real
  Keychain storage.
- **codex-only backend.** No Claude backend. A `crate::model::AuthMode` enum and TODO
  seams in `agent/spawn.rs`, `agent/mod.rs`, `agent/preflight.rs` mark where an
  **API-key auth mode** would slot in later. Not implemented.
- **No SSH tunnel yet.** Private/VPC databases requiring a bastion are not reachable in
  this MVP (russh integration is deferred).
- **No MCP grounding yet.** Schema context is sent in-prompt only (redacted DDL summary,
  no row data). The opt-in read-only MCP introspection server is deferred.
- **Cloud-provider tuning is partial.** Supabase pooler, Neon, PlanetScale, and RDS get
  basic host detection + tuning; not every provider gotcha is covered. No bundled CA
  files — supply a custom CA per connection via `extraParams["sslrootcert"]`.
- **Results grid is a simple windowed table**, not a virtualized grid. Fine for typical
  result sets; swap in a virtualizer if you routinely pull 100k+ rows.
- **Audit log is tamper-EVIDENT, not tamper-proof.** The SHA-256 hash-chain detects
  post-hoc edits but not a determined rewrite by someone with write access to app.db.

## Safety model (unchanged by this build)

Four layers, per `ARCHITECTURE.md` §4:
- **L1** parse/classify (sqlparser, all engines) — UX pre-filter.
- **L2** DB-enforced read-only session — the **authoritative** boundary.
- **L3** impact preview (EXPLAIN for reads; execute-in-txn + rollback for writes, gated
  above a row-estimate threshold).
- **L4** human approval gate — writes/DDL are always hard-gated and additionally require
  the connection's `allow_writes` to be on (default off).
