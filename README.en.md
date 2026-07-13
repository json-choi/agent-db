# DopeDB

DopeDB is a **free, open-source desktop app that gives AI agents a safe path to your databases**. Through MCP, agents can inspect schemas, run read queries, and understand results, while raw credentials, read-only enforcement, write approvals, rollback previews, and audit logs stay under the control of the local app.

- Website: https://dopedb.dev (Korean: https://dopedb.dev/?lang=ko)
- Download: [Windows x64](https://github.com/json-choi/dopedb/releases/latest/download/DopeDB-windows-x64-setup.exe) · [macOS Apple Silicon](https://github.com/json-choi/dopedb/releases/latest/download/DopeDB-macos-arm64.dmg) · [macOS Intel](https://github.com/json-choi/dopedb/releases/latest/download/DopeDB-macos-x64.dmg)
- Korean: [README.md](./README.md)
- Project docs: [docs/PROJECT.md](./docs/PROJECT.md)

## Features

- PostgreSQL, MySQL/MariaDB, and SQLite connection management
- Built-in MCP server that gives existing agents a guarded database surface
- Read-only defaults and SQL classification
- Approval card plus `allow_writes` gate for writes and DDL
- Query history and hash-chained audit log
- Live in-app view of agent query results
- Korean/English support across the marketing site, desktop client UI, and GitHub README
- macOS/Windows downloads and Tauri updater metadata through GitHub Releases

## Why DopeDB

There are great free database clients, and there are plenty of AI SQL generators. DopeDB closes the risky gap between them.

- It is not an AI feature bolted onto a SQL editor. It is a **local database gateway your existing agent can use through MCP**.
- The agent does not receive raw database credentials; the local app owns connections and secrets.
- The MCP tool surface is read-only today. Every query first passes through `plan_query`, which returns EXPLAIN and aggregate database-health cautions before execution. Writes and DDL stay behind a human-visible approval gate.
- The context your agent saw, the queries it ran, the results, approvals, and audit logs land in a UI humans can review.

## Language Support

- Website: use the top-right language switcher or `?lang=ko` / `?lang=en`
- Desktop client: choose Korean or English from Settings -> Language
- GitHub README: [Korean](./README.md) / [English](./README.en.md)

The current MCP tool surface is read-only. MCP write tools are not shipped yet; manual writes in the desktop UI remain behind approval gates.

## Development

Requirements:

- Rust stable 1.82 or newer
- Node.js 24
- pnpm 10.26.1
- Xcode Command Line Tools for macOS builds

```sh
pnpm install
pnpm tauri dev
```

Useful checks:

```sh
pnpm build
pnpm site:build
pnpm build:bridge
cargo check --workspace
```

## Releases

Pushing an `app-v*` tag starts the GitHub Actions release workflow. It builds Apple Silicon and Intel macOS artifacts, a Windows x64 NSIS installer, and uploads updater metadata to GitHub Releases. The website download buttons point directly at stable release asset names: `DopeDB-windows-x64-setup.exe`, `DopeDB-macos-arm64.dmg`, and `DopeDB-macos-x64.dmg`.

```sh
git tag app-v0.1.1
git push origin app-v0.1.1
```

The release workflow requires the `TAURI_SIGNING_PRIVATE_KEY` repository secret.

## macOS Warning

Until the app is signed and notarized with an Apple Developer ID, macOS can show an unidentified developer warning. After confirming the file came from GitHub Releases, open System Settings -> Privacy & Security -> Open Anyway.

If you need to remove the quarantine flag from Terminal, copy DopeDB to Applications first, then run:

```sh
sudo xattr -dr com.apple.quarantine /Applications/DopeDB.app
open /Applications/DopeDB.app
```

Replace `/Applications/DopeDB.app` if the app lives somewhere else. This command removes the macOS quarantine flag from the downloaded app, so only use it for files you verified came from the official GitHub Release.

## License

MIT License. See [LICENSE](./LICENSE).
