# DopeDB 클라이언트

Tauri v2 기반 데이터베이스 클라이언트. React/TS 프론트 + Rust 코어, MCP로 에이전트에 안전하게 DB를 노출한다.

## AI 작업자 필수 협업 규칙

이 규칙은 Claude Code에 필수다. 저장소 전체 AI 정책은 `AGENTS.md`, 사람용 절차는 `CONTRIBUTING.md`에 있으며, 협업·배포 정책을 바꿀 때는 세 파일을 같은 변경에서 함께 갱신한다.

1. 작업 전에 `gh api user --jq .login`으로 현재 GitHub 계정을 확인하고 `git status`로 다른 작업자의 변경이 없는지 확인한다.
2. 일반 작업 브랜치는 반드시 `work/<정확한-GitHub-login>/<짧은-작업명>`을 사용한다. login은 대소문자까지 실제 계정과 같아야 한다.
3. 본인의 `work/` 브랜치만 push하고 PR 대상은 `main`으로 한다. `main`이나 다른 작업자의 브랜치에 직접 push하지 않고, 보호 규칙·실패한 CI·필수 리뷰를 우회하지 않는다.
4. `json-choi`만 사용자의 명시적인 요청이 있을 때 관리/bootstrap 작업을 위해 `main` 관리자 bypass를 사용할 수 있다. 일반 개발에는 사용하지 않는다.
5. Actions, 버전 파일, `AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`는 `@json-choi` CODEOWNERS 대상이다. 보호 범위를 약화하지 않는다.

본인 카나리는 작업 브랜치를 push한 뒤 보호된 `main`의 workflow를 실행한다.

```sh
branch="$(git branch --show-current)"
git push -u origin "$branch"
gh workflow run canary.yml --ref main -f source_ref="$branch"
```

카나리는 `work/${GITHUB_ACTOR}/`로 시작하는 본인 브랜치만 허용한다. unsigned 내부 테스트 prerelease이며 updater signing key, updater artifact/signature, 고정 다운로드 alias, `latest.json`을 절대 넣지 않는다. 카나리 태그나 Release를 수동으로 만들지 않는다.

정식 버전은 `json-choi`가 명시적으로 요청받은 경우에만 발행한다. 비소유자는 정식 태그 생성, Release 발행, `stable-release` 승인, release workflow/환경/ruleset/secret 변경을 시도하지 않는다. GitHub 개인 저장소 collaborator에게 Release UI/API 권한 자체가 남는다는 제약이 있으므로, 그 권한까지 제거됐다고 안내하지 않는다. 공식 정식 경로만 owner-only 태그·환경 승인·workflow·immutable release로 통제된다.

## 아키텍처 지도

- `src/screens/`: 화면 단위 폴더 — 탭 하나 = 폴더 하나. Settings처럼 하위 섹션이 있으면 부모 폴더 아래 같은 패턴으로 중첩(`Settings/Mcp` 등).
- `src/components/`: 여러 화면이 공유하는 UI 조각.
- `src/lib/`: 렌더 마크업 없는 순수 로직/헤드리스 상태(i18n, agentFeed 등).
- `src/lib/queries.ts` + `queryClient.tsx`: TanStack Query 기반 앱 전역 읽기 캐시. 백엔드 읽기는 전부 여기 등록된 쿼리로 접근한다.
- `src/ipc/`: Tauri invoke 래퍼(`commands.ts`)와 Rust 데이터 계약 미러(`types.ts`).
- `src/design-system/`: 토큰(`tokens.css`)과 공통 클래스(`system.css`) — 상세는 `src/design-system/README.md`.
- `src-tauri/src/`: `connection`, `introspect`, `executor`, `migrations`, `safety`, `audit`, `mcp`, `store`, `commands` 도메인 모듈 + `model.rs`(데이터 계약).
- `dopedb-mcp-stdio/`, `site/`: 별개 하위 프로젝트(각자 자체 빌드).

## 빌드 · 검증

- `pnpm build` — tsc + vite build.
- `pnpm dev:app` — Rust 빌드 후 앱 실행.
- `cargo test --manifest-path src-tauri/Cargo.toml` — Rust 테스트.

## 릴리스

- 태그는 **반드시 `app-v0.0.0` 형식**이다. `.github/workflows/release.yml`이 `app-v*`에만 반응하므로 `v0.0.0`으로 달면 릴리스가 조용히 안 나간다(0.1.7·0.1.8이 이렇게 유실됐다).
- 버전은 `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`(+`Cargo.lock`) 네 곳을 함께 올리고, 범프는 기능 커밋에 같이 싣는다.
- 정식 태그는 `main`에 합쳐진 커밋에 저장소 소유자만 만들고, `stable-release` 환경 승인 뒤 배포한다. macOS/Windows 3종을 draft에 모두 올린 다음 공개하며, 공개 후 태그와 asset은 immutable이다.
- 협업자 브랜치는 `work/<GitHub아이디>/<작업명>`을 쓰고, 본인 브랜치만 `.github/workflows/canary.yml`로 unsigned canary prerelease를 만들 수 있다. 카나리는 updater signing key와 `latest.json`을 절대 사용하지 않는다.
- `canary-*`를 제외한 모든 태그는 `owner-only-tags-except-canary` ruleset으로 `json-choi`만 생성·수정·삭제할 수 있다. 우회하지 않는다.
- release immutability는 2026-07-13 이후 발행된 release에 적용되며 이전 release에는 소급되지 않는다.

## 컨벤션

**네이밍**: `components/*.tsx`는 PascalCase, 컴포넌트당 1파일(CSS 필요시 동명 `ComponentName.css` 동일 폴더). `screens/Folder/index.tsx` + `folder.css`(소문자, 폴더명과 동일), 중첩 screens(`Settings/Mcp` 등)도 동일 패턴. `lib/*.ts(x)`는 camelCase, 유틸/헤드리스 상태. `src-tauri/src/**/*.rs`는 snake_case, 도메인폴더/`mod.rs` + 형제 서브모듈.

**export**: 메인 산출물이 하나면 default export. 서로 다른 산출물이 둘 이상(훅+프로바이더, barrel 등)이면 전부 named로 통일하고 default 없음. 단일 default 파일도 보조 타입은 named로 함께 export 가능. `lib/*.ts(x)`는 export 개수와 무관하게 항상 named(default 금지).

**import 순서**: `react` → 기타 외부 패키지(`@tauri-apps/*`) → `../../ipc/commands` → `../../ipc/types`(타입 먼저) → `../../components/*` → `../../lib/*` → 자기 폴더 `./*.css`(항상 마지막, 예외 없음).

**화면 추가**: `screens/X/index.tsx` + `x.css` 생성 → `App.tsx`에 탭 등록. 하위 화면(Settings 등)은 부모 폴더 아래 같은 패턴 중첩.

**컴포넌트 추가**: `components/PascalCase.tsx`, 자체 렌더 마크업이 있으면 동명 `.css` 동반. 여러 컴포넌트가 공유하는 스타일만 예외적으로 `grid.css`처럼 공용 파일에 둔다.

**IPC 추가**: `src-tauri/src/commands/mod.rs`에 커맨드 추가 → `src/ipc/commands.ts`에 invoke 래퍼 추가 → 새 타입은 `src-tauri/src/model.rs`에 정의하고 `src/ipc/types.ts`에 1:1 미러(`snake_case` → `camelCase`만 다르게, 필드 순서 동일). 두 파일 모두 상단에 "이 파일이 데이터 계약의 authoritative source/mirror"라는 주석을 유지한다. `model.rs` 밖 타입(예: `introspect/mod.rs`, `mcp/connect.rs`)도 `types.ts`에 모으고 `// mirrors src-tauri/src/x.rs` 주석으로 출처를 명시한다.

**i18n**: `en`+`ko` 둘 다 필수. 키는 항상 `namespace.camelCaseKey`(2세그먼트). namespace는 화면/컴포넌트 이름과 1:1(`connections`, `sql`, `mcp`, `safety`, `rowEditor` 등). `common`, `app`만 전역 공유 네임스페이스 예외. 사전 내 알파벳 정렬 유지.

**CSS**: 토큰(`--ds-*`)만 사용, hex 직접 사용 금지. 카드/패널/버튼/배지 등은 정본 클래스(`.card`, `.ds-panel`, `.btn`, `.badge`, `.ds-toolbar` 등, `src/design-system/README.md` 참고) 재사용.

**Rust 주석**: `src-tauri/src/**/*.rs`는 파일 최상단에 `//!` 모듈 doc comment 필수(`main.rs`만 템플릿 보일러플레이트라 `//` 예외). `pub` 아이템에는 `///` doc comment를 붙이는 경우가 많다.

**TS/TSX 헤더**: 45줄 넘는 화면/컴포넌트/lib 파일은 첫 import 이전에 1~3줄 `//` 주석으로 파일의 역할과 설계 의도를 설명한다. 20줄 이하 자명한 소형 파일은 생략 가능.

**lib/ vs components/**: 자체 DOM/CSS를 렌더하면 `components/`, `{children}`만 감싸고 상태/이벤트/컨텍스트 계산만 하면 `lib/`(예: `agentFeed`, `i18n`은 lib; `Toast`는 자체 DOM+CSS를 렌더하므로 components).

**데이터 로딩**: 화면에서 `useEffect` + `invoke`로 직접 fetch하지 않는다. `lib/queries.ts`에 쿼리 옵션(키 + queryFn + staleTime)을 추가하고 화면은 `useQuery`/`useQueries`로 읽는다. 백엔드 이벤트로 인한 캐시 무효화는 `lib/queryClient.tsx` 한 곳에 모은다. 캐시가 비어 있는 최초 로딩에만 `<Skeleton />`(200ms 지연 노출)을 쓰고, 재검증 중에는 이전 데이터를 유지한다.

## 함정

- Tauri v2 이벤트 이름에 `.`을 쓰면 emit이 조용히 실패한다(`:` 등으로 구분자 대체).
- `NUMERIC`/`MONEY` 컬럼 값은 정밀도 보존을 위해 문자열로 직렬화된다. 숫자로 바로 캐스팅하지 말 것.
