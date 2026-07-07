# dopedb

dopedb is an open-source macOS database client that runs locally. AI tools connect to dopedb over MCP, while dopedb keeps database connections, read-only execution, approval gates, and audit logs inside the desktop app.

- Website: https://dopedb.dev
- Download: https://github.com/json-choi/dopedb/releases/latest
- Korean: [README.md](./README.md)
- Project docs: [docs/PROJECT.md](./docs/PROJECT.md)

## Features

- PostgreSQL, MySQL/MariaDB, and SQLite connection management
- Read-only defaults and SQL classification
- Approval card plus `allow_writes` gate for writes and DDL
- Query history and hash-chained audit log
- Local MCP server for read tools from Claude Code, Claude Desktop, Codex, and similar clients
- macOS downloads and Tauri updater metadata through GitHub Releases

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

Pushing an `app-v*` tag starts the GitHub Actions release workflow. It builds Apple Silicon and Intel macOS artifacts and uploads updater metadata to GitHub Releases.

```sh
git tag app-v0.1.1
git push origin app-v0.1.1
```

The release workflow requires the `TAURI_SIGNING_PRIVATE_KEY` repository secret.

## macOS Warning

Until the app is signed and notarized with an Apple Developer ID, macOS can show an unidentified developer warning. After confirming the file came from GitHub Releases, open System Settings -> Privacy & Security -> Open Anyway.

## License

MIT License. See [LICENSE](./LICENSE).
