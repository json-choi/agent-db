# dopedb

자연어로 데이터베이스를 다루는 macOS 네이티브 DB 클라이언트. **이미 구독 중인** AI 에이전트
CLI(Claude Code / OpenAI Codex)를 백엔드로 구동하되, 연결·자격증명·엄격한 안전 파이프라인은
dopedb가 직접 소유합니다:
**기본 읽기 전용 · 사람 승인 게이트 · 전체 감사 로그 · 롤백 미리보기 기반 트랜잭션 쓰기.**

> **상태:** 동작하는 프로토타입 (Tauri v2 + React/TS). 백엔드 에이전트는 **codex 전용**(ChatGPT 구독).

## 실행 방법

**반드시 `pnpm tauri dev`(타우리 데브)로 실행해야 합니다.** Vite 프론트엔드와 Rust 코어를
함께 띄우고 핫 리로드가 동작하는 유일한 실행 방식입니다.

사전 준비물:
- **Rust** (stable ≥ 1.82)
- **Node** ≥ 18 + **pnpm** (`corepack enable pnpm`)
- **Xcode Command Line Tools** (`xcode-select --install`)
- **codex CLI** — ChatGPT 구독으로 로그인된 상태여야 함
  (`codex login` 실행 후 `codex login status`가 *Logged in using ChatGPT* 라고 나와야 함)

```sh
pnpm install
pnpm tauri dev      # 타우리 데브 — Vite 프론트 + Rust 코어, 핫 리로드
```

배포용 `.dmg` 빌드: `pnpm tauri build`.

MCP stdio 브리지 사이드카, 레이어별 개별 실행, 패키징 세부사항은 [BUILD.md](./BUILD.md) 참고.

## 다운로드와 업데이트

다운로드는 GitHub Releases의 최신 릴리스를 사용합니다:
https://github.com/json-choi/dopedb/releases/latest

현재는 Apple Developer ID로 서명/공증된 배포가 아니면 macOS에서 "developer cannot be
verified" 또는 "unidentified developer" 경고가 뜰 수 있습니다. GitHub Releases에서 받은 파일인지
확인한 뒤 다음 방법으로 열 수 있습니다.

1. 앱을 한 번 실행해 macOS 경고를 확인합니다.
2. System Settings → Privacy & Security로 이동합니다.
3. Security 영역에서 dopedb의 `Open Anyway`를 누릅니다.
4. 다시 뜨는 확인 창에서 `Open`을 누릅니다.

Finder에서 앱을 Control-click 또는 우클릭한 뒤 `Open`을 선택해 예외 승인할 수도 있습니다.
Apple도 신뢰할 수 있는 출처이고 변조되지 않았다고 확신할 때만 이 보안 예외를 쓰라고 안내합니다:
https://support.apple.com/102445

새 버전 릴리스는 `app-v*` 태그 push로 자동 빌드됩니다. 릴리즈 워크플로우가 macOS 번들,
업데이트 번들, `latest.json`을 GitHub Release asset으로 올리고, 클라이언트는
Settings → Updates에서 새 버전을 감지하면 업데이트 버튼을 활성화합니다.

릴리즈 전에 GitHub repository secret `TAURI_SIGNING_PRIVATE_KEY`를 설정해야 합니다.
자세한 릴리즈 절차는 [BUILD.md](./BUILD.md) 참고.

## 웹사이트

오픈소스 랜딩 페이지는 Next.js 앱으로 [site](./site)에 있습니다.

```sh
pnpm --dir site install
pnpm site:preview-image
pnpm site:dev
```

Vercel에서는 Root Directory를 `site`로 설정하면 됩니다.

## 설계 문서
- [ARCHITECTURE.md](./ARCHITECTURE.md) — 시스템 설계, 에이전트 브리지, 4계층 안전 모델, 기술 스택, 리스크.
- [ROADMAP.md](./ROADMAP.md) — 단계별 빌드 계획 (Phase 0 리스크 제거 스파이크 → MVP → v1) + 레포 구조.
- [DESIGN-REVIEW.md](./DESIGN-REVIEW.md) — 빌드 전 적대적 리뷰. **코드 작성 전에 읽을 것.**

## License

MIT License. See [LICENSE](./LICENSE).

## Phase 1 착수 전 결정 사항 (설계 리뷰에서)
1. **비용 구조 / 기본 백엔드.** 2026년 중반 기준 조사에 따르면 `claude -p`는 대화형 구독이 아닌
   *별도 과금되는 Agent-SDK 크레딧 풀*에서 차감되는 반면, `codex exec`는 여전히 ChatGPT 구독
   범위에서 사용됩니다. 이는 Claude 쪽 "API 키 없이 구독만으로" 논리를 약화시키며 **codex를 기본값**
   으로 두는 근거가 됩니다. → 방향 확정 전에 **개정된 Phase 0 스파이크**에서 실제 구독으로 실증 검증할 것.
2. **에이전트 도구 잠금.** 스폰되는 CLI는 완전한 에이전트(셸/파일시스템/네트워크)입니다. **정제된
   환경변수**와 함께 "텍스트만 출력"하도록 축소해야 하며, 그렇지 않으면 "에이전트는 SQL만 제안한다"는
   경계가 *강제되지* 않습니다 — `~/.pgpass`/환경변수 DSN을 읽어 감사 로그 밖에서 직접 연결할 수 있음.
3. **쓰기 미리보기 영향 범위.** execute-후-`ROLLBACK` 미리보기는 실제로 문장을 실행하며 라이브
   테이블에 락을 겁니다 — EXPLAIN 행 추정치로 상한을 두고, 임계치 초과 시 추정치만 표시할 것.
4. 최소 권한 DB 역할 (자동 생성 vs 요구) · 읽기 자동 실행 vs 전부 게이트.

전체 목록과 **조건부 Go** 판정(Phase 1 인력 투입 전 개정 Phase 0 스파이크 수행)은
[DESIGN-REVIEW.md](./DESIGN-REVIEW.md) → "Must-answer before Phase 1" 참고.
