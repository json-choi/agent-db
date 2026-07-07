# dopedb Project Guide

This is the single maintained project document for dopedb. Keep the root README files short and update this file when architecture, release, or safety behavior changes.

## Product

dopedb is a local-first macOS database client built with Tauri. It lets a user inspect and operate databases manually, and it exposes a local MCP server so AI tools can safely inspect connected databases without receiving raw credentials.

Current scope:

- Desktop app: Tauri v2, Rust core, React UI, Vite
- Landing site: Next.js under `site/`, hosted at https://dopedb.dev
- Databases: PostgreSQL, MySQL/MariaDB, SQLite
- MCP: local Streamable HTTP endpoint plus stdio bridge
- Distribution: GitHub Releases and Tauri updater metadata

## Architecture

The Rust core owns the trust boundary:

- `connection/`: connection profiles, pools, provider tuning, and Keychain-backed secrets
- `safety/`: SQL classification, read-only enforcement, preview, and approval policy
- `executor/`: read execution and gated write execution
- `audit/`: query history and hash-chained audit records
- `mcp/`: local MCP server, stdio bridge listener, tool handlers, and client config helpers
- `store/`: local SQLite app store under the platform app data directory

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
- `run_query`

All current MCP tools are read-only. `run_query` rejects non-read SQL before execution, and the database read-only session is the authoritative guard. MCP write tools are intentionally deferred.

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

- Rust stable 1.82 or newer
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

Release runs on `app-v*` tags or manual dispatch:

- build macOS Apple Silicon artifact
- build macOS Intel artifact
- upload installers, updater archives, signatures, and `latest.json`

Required GitHub secret:

```txt
TAURI_SIGNING_PRIVATE_KEY
```

The local updater key path used during setup was `~/.tauri/dopedb-updater.key`. Do not commit private keys.

## Dependency Policy

Use the latest compatible patch/minor versions by default. Major upgrades are allowed when they compile cleanly and do not change security boundaries.

Current deliberate holds:

- `sqlx 0.9`: requires Rust 1.94, while the app currently supports Rust 1.82.
- `rmcp 2.x`: API migration should be reviewed separately because it touches the MCP server boundary.
- `keyring 4.x`: review migration separately because it touches credential storage.
- `sqlparser 0.62`: review parser behavior separately because it affects SQL classification.
- `pnpm 11`: current supply-chain policy rejected a same-day transitive package in the site lockfile; stay on pnpm 10.26.1 until that policy is intentionally configured.

## macOS Distribution

The app is currently distributed outside the Mac App Store. Until Developer ID signing and notarization are configured, macOS can show an unidentified developer warning. Users should only bypass the warning after confirming the file came from the official GitHub Release.

User-facing bypass path:

1. Try opening dopedb once.
2. Open System Settings -> Privacy & Security.
3. Choose Open Anyway for dopedb.
4. Confirm Open.

## Deferred Work

- Developer ID signing and notarization
- MCP write proposal tool with in-app approval round trip
- Token rotation UI for MCP configs
- SSH tunnel support
- More granular MCP client origin handling
- Virtualized result grid for very large result sets
