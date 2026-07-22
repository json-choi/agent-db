# DopeDB Project Guide

This is the single maintained project document for DopeDB. Keep the root README files short and update this file when architecture, release, or safety behavior changes.

## Product

DopeDB is a local-first desktop database client built with Tauri. It lets a user inspect and operate databases manually, and it exposes a local MCP server so AI tools can safely inspect connected databases without receiving raw credentials.

Current scope:

- Desktop app: Tauri v2, Rust core, React UI, Vite
- Landing site: Next.js under `site/`, hosted at https://dopedb.dev
- Databases: PostgreSQL, MySQL/MariaDB, SQLite
- MCP: local Streamable HTTP endpoint plus stdio bridge
- Distribution: GitHub Releases and Tauri updater metadata

Planned team collaboration, workspace-scoped provider integrations, shared
connections, dashboards, and saved agent analysis are specified in the
[Workspace Collaboration Roadmap](./WORKSPACE_ROADMAP.md).

## Architecture

The Rust core owns the trust boundary:

- `driver/`: driver catalog, compatibility/recommendation, install state, and runtime dispatch
- `connection/`: connection profiles, concrete pools, provider tuning, and OS credential-store-backed secrets
- `safety/`: SQL classification, read-only enforcement, preview, and approval policy
- `executor/`: read execution and gated write execution
- `audit/`: query history and hash-chained audit records
- `mcp/`: local MCP server, stdio bridge listener, tool handlers, and client config helpers
- `store/`: local SQLite app store under the platform app data directory, including
  connection-scoped saved dashboard definitions

The frontend renders database state and approval decisions. It does not own the safety decision. Writes and DDL require the Rust path to see both explicit approval and `allow_writes = true`.

## MCP Behavior

The app starts two loopback listeners:

- HTTP MCP endpoint: `http://127.0.0.1:7686/mcp`
- stdio bridge TCP listener: `127.0.0.1:7687`

The bundled `dopedb-mcp-stdio` sidecar reads `~/Library/Application Support/dopedb/mcp.json`, connects to the running app, and pumps stdio bytes for clients that cannot call localhost HTTP directly.

Current MCP tools:

- `list_connections`
- `list_tables`
- `describe_table`
- `plan_query`
- `run_query`
- `create_dashboard`

All target-database access exposed by MCP is read-only. Every data query is a mandatory two-step operation: `plan_query` validates one SELECT, runs non-executing EXPLAIN, gathers aggregate database-pressure signals, and returns a 30-second single-use `planId`; `run_query` accepts only that id, not replacement SQL or a connection. This forces the agent to receive the current warnings before execution. The database read-only session remains the authoritative guard. Each successful query returns a durable `queryRunId`. After explicit user agreement, `create_dashboard` must reference that exact ID; DopeDB loads the connection and SQL from the successful agent history row instead of accepting replacements from the agent. Dashboard creation only writes to DopeDB's local app store and never to the target database. MCP target-database write tools are intentionally deferred.

Query planning never sends other sessions' SQL text, users, client addresses, or parameters to the agent. It returns aggregate connection usage, active/long-running query counts, lock-wait counts, and replication lag when the engine exposes them. PostgreSQL connections can grant/revoke the built-in `pg_monitor` role from Safety settings through one fixed, explicitly confirmed command. This narrow command is audited separately and does not enable arbitrary writes. Without that role, planning reports limited monitoring coverage and applies a caution decision. MySQL uses available Performance Schema aggregates; SQLite reports basic local coverage.

Saved dashboards belong to a connection and persist in `app.db`, so they are restored when that connection is opened again. A dashboard stores SQL plus a bounded, versioned declarative visualization (`auto`, `metric`, `line`, `bar`, or `table`); it does not store generated HTML. Opening a dashboard calls the dedicated `run_dashboard` command, which reloads and revalidates the definition against the current engine and always uses the L2 read-only session independently of write/auto-run settings. Result rows are never persisted.

Supported client helpers in the app:

- Claude Code direct HTTP
- Claude Desktop stdio bridge
- Codex stdio bridge
- Manual HTTP snippets for other MCP clients

## Safety Model

The important rules are enforced in Rust:

- Reads run through read-only database sessions.
- Writes are off by default per connection.
- A write or DDL path requires `allow_writes = true`.
- Manual writes require an approval card unless the connection policy explicitly disables approval.
- Migrations also run through the same write gate.
- Successful and blocked execution paths are audited.

MCP annotations and prompts are treated as hints, not security boundaries.

## Development

Required local tools:

- Rust stable 1.94 or newer
- Node.js 24
- pnpm 10.26.1
- Xcode Command Line Tools

Main commands:

```sh
pnpm install
pnpm tauri dev
pnpm build
pnpm site:build
pnpm build:bridge
cargo check --workspace
```

The bridge sidecar must exist before Tauri build scripts validate `bundle.externalBin`. `pnpm build:bridge` stages the host binary into `src-tauri/binaries/`.

## Landing Site

The site lives in `site/`.

- Canonical domain: https://dopedb.dev
- Framework: Next.js app router
- SEO files: `site/app/robots.ts`, `site/app/sitemap.ts`
- Product preview image: `site/public/dopedb-dashboard.png`
- Preview generator: `site/scripts/generate-preview.py`

Local commands:

```sh
pnpm site:preview-image
pnpm site:dev
pnpm site:build
```

Vercel should use `site` as the root directory.

## CI and Releases

CI runs on pull requests and `main` pushes:

- install root and site dependencies
- build desktop frontend
- build landing site
- stage MCP bridge sidecar
- run `cargo check --workspace`

Stable release runs only on an owner-created `app-v*` tag whose commit is already in `main` and whose version matches `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `Cargo.lock`. The `stable-release` environment requires approval from `@json-choi` before the signing key and write token are available:

- build macOS Apple Silicon artifact
- build macOS Intel artifact
- build Windows x64 NSIS installer with `src-tauri/tauri.windows.conf.json`
- upload stable direct-download aliases:
  `DopeDB-windows-x64-setup.exe`, `DopeDB-macos-arm64.dmg`, `DopeDB-macos-x64.dmg`
- upload installers, updater archives, signatures, and `latest.json`
- keep the release as a draft until every matrix build and stable alias upload succeeds, then publish it for immutable tag and asset protection

Contributors use `work/<github-login>/<topic>` branches and may manually dispatch `.github/workflows/canary.yml` from `main` for their own branch only. Canary builds publish through a per-user `canary-<github-login>` environment as unsigned prereleases without updater artifacts, updater signatures, or `latest.json`. See `CONTRIBUTING.md` for the exact commands.

Required GitHub secret:

```txt
TAURI_SIGNING_PRIVATE_KEY
```

The local updater key path used during setup was `~/.tauri/dopedb-updater.key`. Do not commit private keys.

## Dependency Policy

Use the latest stable compatible library versions, including major releases, and
update the affected safety tests whenever an upgrade changes parser, database, MCP,
or credential-store behavior. The desktop currently builds with TypeScript 7; the
two Next.js apps use TypeScript 6.0.3 because Next.js 16.2.11 cannot load TypeScript
7's new API yet.

Current non-library tooling hold:

- `pnpm 11`: current supply-chain policy rejected a same-day transitive package in the site lockfile; stay on pnpm 10.26.1 until that policy is intentionally configured.

## macOS Distribution

The app is currently distributed outside the Mac App Store. Until Developer ID signing and notarization are configured, macOS can show an unidentified developer warning. Users should only bypass the warning after confirming the file came from the official GitHub Release.

User-facing bypass path:

1. Try opening DopeDB once.
2. Open System Settings -> Privacy & Security.
3. Choose Open Anyway for DopeDB.
4. Confirm Open.

Terminal alternative after copying the app to Applications:

```sh
sudo xattr -dr com.apple.quarantine /Applications/DopeDB.app
open /Applications/DopeDB.app
```

Only document this command with the release-origin warning. It removes the macOS quarantine flag from the downloaded app and should not be presented as a general bypass for untrusted binaries.

## Deferred Work

- Developer ID signing and notarization
- MCP write proposal tool with in-app approval round trip
- Token rotation UI for MCP configs
- SSH tunnel support
- More granular MCP client origin handling
- Virtualized result grid for very large result sets
