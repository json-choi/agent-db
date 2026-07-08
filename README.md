# dopedb

로컬에서 실행되는 오픈소스 macOS 데이터베이스 클라이언트입니다. AI 도구는 MCP로 dopedb에 연결하고, dopedb는 연결 정보, 읽기 전용 실행, 승인 게이트, 감사 로그를 앱 안에서 관리합니다.

- 웹사이트: https://dopedb.dev
- 다운로드: https://github.com/json-choi/dopedb/releases/latest
- English: [README.en.md](./README.en.md)
- 상세 문서: [docs/PROJECT.md](./docs/PROJECT.md)

## 주요 기능

- PostgreSQL, MySQL/MariaDB, SQLite 연결 관리
- 기본 읽기 전용 실행과 SQL 분류
- 쓰기/DDL 실행 전 승인 카드와 `allow_writes` 게이트
- 쿼리 히스토리와 hash-chain 감사 로그
- 로컬 MCP 서버: Claude Code, Claude Desktop, Codex 등에서 읽기 도구 사용
- GitHub Releases 기반 macOS 다운로드와 Tauri updater

현재 MCP 도구는 읽기 전용입니다. MCP 쓰기 도구는 아직 제공하지 않으며, 수동 UI의 쓰기 기능만 승인 게이트 뒤에서 동작합니다.

## 개발 실행

필요한 도구:

- Rust stable 1.82 이상
- Node.js 24
- pnpm 10.26.1
- macOS 빌드용 Xcode Command Line Tools

```sh
pnpm install
pnpm tauri dev
```

개별 검증:

```sh
pnpm build
pnpm site:build
pnpm build:bridge
cargo check --workspace
```

## 릴리스

`app-v*` 태그를 push하면 GitHub Actions가 macOS Apple Silicon/Intel 빌드와 updater metadata를 GitHub Release에 업로드합니다.

```sh
git tag app-v0.1.1
git push origin app-v0.1.1
```

릴리스 워크플로우에는 `TAURI_SIGNING_PRIVATE_KEY` repository secret이 필요합니다.

## macOS 경고

Apple Developer ID로 서명/공증하기 전에는 macOS가 개발자 확인 경고를 표시할 수 있습니다. GitHub Releases에서 받은 파일인지 확인한 뒤 System Settings -> Privacy & Security -> Open Anyway로 실행을 허용할 수 있습니다.

터미널로 quarantine 플래그를 해제해야 한다면, dopedb를 Applications 폴더에 복사한 뒤 아래 명령을 실행하세요.

```sh
sudo xattr -dr com.apple.quarantine /Applications/dopedb.app
open /Applications/dopedb.app
```

`/Applications/dopedb.app`이 아니라면 실제 앱 경로로 바꾸세요. 이 명령은 macOS가 다운로드 파일에 붙인 격리 플래그를 제거하므로, 공식 GitHub Release에서 받은 파일에만 사용하세요.

## 라이선스

MIT License. [LICENSE](./LICENSE)를 참고하세요.
