# DopeDB (도프디비)

DopeDB(도프디비)는 **AI 에이전트에게 안전한 데이터베이스 통로를 열어주는 무료 오픈소스 데스크톱 앱**입니다. MCP를 통해 에이전트는 스키마를 살피고 읽기 쿼리를 실행하고 결과를 이해할 수 있습니다. 원본 인증 정보, 읽기 전용 실행, 쓰기 승인, 롤백 미리보기, 감사 로그는 로컬 앱이 통제합니다.

- 웹사이트: https://dopedb.dev/ko (English: https://dopedb.dev)
- 다운로드: [Windows x64](https://github.com/json-choi/dopedb/releases/latest/download/DopeDB-windows-x64-setup.exe) · [macOS Apple Silicon](https://github.com/json-choi/dopedb/releases/latest/download/DopeDB-macos-arm64.dmg) · [macOS Intel](https://github.com/json-choi/dopedb/releases/latest/download/DopeDB-macos-x64.dmg)
- English: [README.en.md](./README.en.md)
- 상세 문서: [docs/PROJECT.md](./docs/PROJECT.md)

## 주요 기능

- PostgreSQL, MySQL/MariaDB, SQLite 연결 관리
- 내장 MCP 서버: 기존 에이전트에 안전한 데이터베이스 접근면 제공
- 기본 읽기 전용 실행과 SQL 분류
- 쓰기/DDL 실행 전 승인 카드와 `allow_writes` 게이트
- 쿼리 히스토리와 hash-chain 감사 로그
- 에이전트 쿼리 결과를 앱 안에서 실시간 확인
- 한국어/영어 지원: 소개 사이트, 데스크톱 클라이언트 UI, GitHub README
- GitHub Releases 기반 macOS/Windows 다운로드와 Tauri updater

## 왜 DopeDB인가

좋은 무료 DB 클라이언트도 있고, AI SQL 생성기도 많습니다. DopeDB는 그 사이의 위험한 빈틈을 메웁니다.

- AI 기능이 붙은 SQL 편집기가 아니라, **기존 에이전트가 MCP로 사용할 수 있는 로컬 DB 게이트웨이**입니다.
- 에이전트에게 원본 인증 정보를 넘기지 않고, 로컬 앱이 연결과 비밀값을 관리합니다.
- 현재 MCP 도구는 읽기 전용입니다. 모든 조회는 실행 전 EXPLAIN과 DB 상태 주의사항을 돌려주는 `plan_query`를 먼저 거칩니다. 쓰기와 DDL은 사람이 보는 승인 게이트 뒤에 둡니다.
- 에이전트가 본 맥락, 실행한 쿼리, 결과, 승인 흐름, 감사 로그를 사람이 검토할 수 있는 UI에 남깁니다.

## 언어 지원

- 소개 사이트: 오른쪽 위 언어 전환 버튼 또는 `?lang=ko` / `?lang=en`
- 데스크톱 클라이언트: Settings -> Language에서 한국어/English 선택
- GitHub README: [한국어](./README.md) / [English](./README.en.md)

현재 MCP 도구는 읽기 전용입니다. MCP 쓰기 도구는 아직 제공하지 않으며, 수동 UI의 쓰기 기능만 승인 게이트 뒤에서 동작합니다.

## 개발 실행

필요한 도구:

- Rust stable 1.94 이상
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

정식 버전은 저장소 소유자만 발행합니다. `main`에 합쳐진 커밋에 소유자가 `app-v*` 태그를 push하고 `stable-release` 환경을 승인하면 GitHub Actions가 macOS Apple Silicon/Intel 빌드, Windows x64 NSIS 설치 파일, updater metadata를 draft release에 모은 뒤 한 번에 공개합니다. 공개된 새 release의 태그와 asset은 immutable release 정책으로 잠깁니다.

```sh
git tag app-v0.1.1
git push origin app-v0.1.1
```

릴리스 워크플로우에는 `TAURI_SIGNING_PRIVATE_KEY` repository secret이 필요합니다. 협업자는 `work/<GitHub아이디>/<작업명>` 브랜치에서 본인 전용 unsigned canary prerelease를 만들 수 있습니다. 브랜치, PR, 카나리 절차는 [CONTRIBUTING.md](./CONTRIBUTING.md)를 참고하세요.

## macOS 경고

Apple Developer ID로 서명/공증하기 전에는 macOS가 개발자 확인 경고를 표시할 수 있습니다. GitHub Releases에서 받은 파일인지 확인한 뒤 System Settings -> Privacy & Security -> Open Anyway로 실행을 허용할 수 있습니다.

터미널로 quarantine 플래그를 해제해야 한다면, DopeDB를 Applications 폴더에 복사한 뒤 아래 명령을 실행하세요.

```sh
sudo xattr -dr com.apple.quarantine /Applications/DopeDB.app
open /Applications/DopeDB.app
```

`/Applications/DopeDB.app`이 아니라면 실제 앱 경로로 바꾸세요. 이 명령은 macOS가 다운로드 파일에 붙인 격리 플래그를 제거하므로, 공식 GitHub Release에서 받은 파일에만 사용하세요.

## 라이선스

MIT License. [LICENSE](./LICENSE)를 참고하세요.
