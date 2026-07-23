# DopeDB 클라이언트 디자인 시스템

이 디자인 시스템은 DataGrip 분석 문서의 결론을 DopeDB 클라이언트에서 바로 사용할 수 있는 토큰과 공통 클래스로 내린 것이다.

핵심 방향:

- DopeDB는 DataGrip 전체 IDE를 복제하지 않는다.
- 공간 규칙은 `explorer -> workbench -> inspector/trust ledger`로 유지한다.
- 모든 데이터 작업은 connection, schema, 실행 주체, safety state를 드러낸다.
- 밀도는 높게 유지하되, agent safety와 audit 정보는 더 명확하게 보이게 한다.

## 파일

- `tokens.css`: 색상, typography, spacing, radius, control size, risk/trust token.
- `system.css`: 버튼, 배지, 패널, toolbar, filter strip, trust/risk surface, object row.
- `index.css`: 클라이언트 import용 entry.
- `src/styles.css`: app shell/layout 전용. 토큰이나 공통 컴포넌트 규칙을 두지 않는다.
- 화면별 CSS: 각 screen/component 옆에 둔다. 예: `screens/Tables/tables.css`, `components/ApprovalCard.css`.

## 토큰 원칙

새 CSS는 `--ds-*` 토큰을 우선 사용한다.

```css
.my-panel {
  padding: var(--ds-space-4);
  border: 1px solid var(--ds-border-subtle);
  border-radius: var(--ds-radius-md);
  background: var(--ds-surface-0);
}
```

예전 전역 변수명인 `--bg`, `--panel`, `--accent`, `--risk-low` 등은 사용하지 않는다. 모든 CSS는 `--ds-*` 토큰만 사용한다. 새 스타일을 추가할 때 이전 변수명을 되살리지 말고 필요한 의미가 없으면 `tokens.css`에 정식 `--ds-*` 토큰을 추가한다.

Vibe coding smell을 줄이기 위해 새 UI에서 아래 패턴은 피한다.

- 화면 CSS에 직접 hex/rgb/rgba 색상, z-index 숫자, 임의 box-shadow를 쓰지 않는다.
- `13px`, `8px`, `12px` 같은 값은 먼저 spacing/type/radius 토큰으로 표현한다.
- 카드, 패널, 코드블록, empty state, toolbar는 기존 정본 클래스(`.card`, `.ds-panel`, `.btn`, `.badge`, `.ds-toolbar` 등, 아래 "공통 클래스" 참고)나 같은 토큰 구조를 재사용한다.
- 한 줄에 여러 선언을 몰아넣지 않는다. 반복 수정이 필요한 화면 CSS는 읽기 쉬운 블록으로 쓴다.

## 미니멀 밀도와 오버플로우

- 기본 화면 패딩은 `--ds-pane-pad`를 사용하고, 반복 카드/패널은 `--ds-space-3` 중심으로 둔다.
- 버튼, 탭, 배지, 헤더 제목, 테이블 셀처럼 폭이 줄어드는 요소는 `min-width: 0`, `overflow: hidden`, `text-overflow: ellipsis`를 갖게 한다.
- 사용자가 읽어야 하는 긴 오류/설명 문구는 `overflow-wrap: anywhere` 또는 `pre-wrap`처럼 의도적으로 줄바꿈한다.
- 새 고정 치수는 가능한 4px 그리드에 맞춘다. 예외가 필요하면 해당 화면 CSS에 이유가 드러나야 한다.
- macOS overlay title bar의 좌상단 제어영역은 `.platform-macos`에서만 활성화되는
  `--ds-window-controls-safe-height`와 `[data-window-controls-safe-zone]` 슬롯으로
  예약한다. rail이나 workspace header가 이 슬롯보다 앞에 컨트롤을 렌더링하지
  않도록 하며 Windows/Linux에는 빈 상단 여백을 만들지 않는다.

## 컬러 팔레트

GitHub Primer 계열의 익숙한 cool-neutral dark 관계를 기준으로 삼고, 색은 의미별로 분리한다. 데스크톱은 OS 설정과 무관하게 같은 다크 팔레트를 사용해 CodeMirror, 데이터 그리드, 팝오버가 서로 다른 테마로 튀지 않게 한다.

- Interaction Blue: 선택, 링크, focus, primary action, 정보.
- Success Green: 성공, 연결됨, trust.
- Caution Amber: review, warning, medium risk.
- Critical Red: destructive, error, blocked, high risk.

새 UI의 임의 색상은 추가하지 않는다. 상호작용에는 `--ds-accent*`, 성공에는 `--ds-success`/`--ds-trust-*`, 경고에는 `--ds-caution`/`--ds-risk-*`, 위험에는 `--ds-critical`/`--ds-danger-*`를 사용한다. 액센트 배경 fill에는 `--ds-accent`, 링크/아이콘/강조 텍스트에는 대비가 더 높은 `--ds-accent-text`를 사용한다.

## 글래스모피즘 표면

앱의 기본 질감은 단단한 작업 표면 위에 restrained glass shell을 얹는 방식이다. Tauri window material은 `src-tauri/tauri.conf.json`의 `windowEffects`에서 정의하고, 앱 내부 표면은 `tokens.css`의 `--ds-surface-*`, `--ds-bg-*`, `--ds-glass-*` 토큰으로만 만든다.

```css
.my-panel {
  background: var(--ds-surface-0);
  border: 1px solid var(--ds-border-subtle);
  box-shadow: var(--ds-shadow-panel);
}
```

- Glass는 app shell, sidebar, header, tabs, modal, popover, toast, empty/skeleton state에만 제한한다.
- 읽고 편집하는 본문, tab body, card, panel, button, form, inspector, data grid는 solid `--ds-surface-*`를 기본값으로 쓴다.
- 데이터 grid는 shell과 cell 모두 blur를 반복하지 않는다.
- 새 화면은 `.card`, `.ds-panel`, `.ds-toolbar`, `.ds-filter-strip`, `.grid-panel` 같은 정본 클래스를 먼저 사용한다. `ds-*` 클래스는 이 앱 클래스들이 공유하는 원시 스타일을 토큰화한 것뿐이며, 대응하는 앱 클래스가 없는 경우(`ds-context-badge`, `ds-workbench-head` 등)에만 `ds-*` 자체가 정본이다.
- 불투명 배경색이 필요하면 하드코딩하지 말고 `tokens.css`에 의미 기반 `--ds-*` 토큰을 추가한다.

## 공통 클래스

정본 이름은 앱에서 실제로 쓰는 클래스(`.btn`, `.badge`, `.card`)다. 대응하는 앱 클래스가 없는 원시 클래스만 `ds-*` 이름 그대로 정본이다. 아래는 `system.css`에 실제로 정의된 클래스만 담는다(더 이상 쓰이지 않는 `ds-button`, `ds-badge`, `ds-status-*`, `ds-stack`, `ds-inline`, `ds-input`, `ds-icon-button`, `ds-grid-shell`, `ds-count`, `ds-page-head`, `ds-surface-grid`, `ds-card-copy`, `ds-inspector`, `ds-code*` 등의 별칭은 제거됐다).

표면(Surface):

- `.card`(`.ds-card`) — 좁은 패딩 카드
- `.ds-panel` — 넓은 패딩 패널(대응하는 앱 클래스 없음)
- `.ds-surface` — 카드/패널과 같은 레시피의 자유 형태 표면
- `.grid-panel`, `.schema-inspector` — 그리드/인스펙터 표면

버튼 & 배지:

- `.btn` (`.primary`, `.danger`, `.small` 조합)
- `.badge` (`.kind`, `.status-ok`/`.risk-low`, `.risk-medium`, `.status-error`/`.status-blocked`/`.risk-high`, `.nowhere` 조합)
- `.ds-context-badge` — connection/schema context pill

워크벤치 헤더:

- `.ds-workbench-head`, `.ds-workbench-title`, `.ds-title-line`
- `.ds-meta-row`, `.ds-meta-dot`
- `.ds-command-group`

툴바 & 필터:

- `.ds-toolbar`(`.grid-toolbar`, `.form-actions`, `.ds-action-row`도 같은 레시피를 공유)
- `.ds-data-toolbar`, `.ds-toolbar-group`, `.ds-toolbar-spacer`
- `.ds-filter-strip`, `.ds-filter-token`

리스트/오브젝트 행:

- `.ds-object-row` (`.active`, `[aria-selected="true"]`)
- `.grid-scroll`

Agent/safety 카드 시스템:

- `.ds-card-grid`, `.ds-card-stack`, `.ds-card-title-row`, `.ds-card-row`
- `.ds-tone-trust`, `.ds-tone-risk`, `.ds-tone-danger`
- `.ds-attention-stack`, `.ds-attention-badge`

폼:

- `.form`, `.form-head`, `.form-close-btn`, `.form-import-row`, `.form-msg`
- input/select/textarea는 `.app` 스코프에서 전역으로 스타일되므로 별도 클래스가 필요 없다.

유틸리티:

- `.muted`, `.error`, `.empty`, `.label`, `.note`
- `.loading`, `.icon`, `.ui-help`, `.icon-only-badge`, `.label-with-help`
- `.ds-engine-mark`

## 사용 예시

```tsx
<section className="ds-panel">
  <header className="ds-workbench-head">
    <div className="ds-workbench-title">
      <h2>Agent trust ledger</h2>
      <p className="muted">What the agent can see and what needs approval.</p>
    </div>
    <span className="ds-context-badge">prod.public</span>
  </header>

  <div className="ds-card-grid">
    <article className="card ds-card-row ds-tone-trust">
      <span className="icon" />
      <div>
        <span className="muted">Schema access</span>
        <strong>Allowed after approval</strong>
      </div>
    </article>
  </div>
</section>
```

## 디자인 규칙

1. Button radius는 6px, panel/card radius는 8px 기준으로 쓴다.
2. 업무용 화면에서는 hero-scale type을 쓰지 않는다. 패널 안 제목은 `h2` 또는 `ds-title` 크기면 충분하다.
3. 카드 안에 카드를 중첩하지 않는다. 패널 안의 반복 항목만 카드로 쓴다.
4. 색은 위험/상태/연결 컨텍스트를 설명할 때만 강하게 쓴다.
5. `Agent` 관련 화면은 chat보다 trust ledger가 먼저 보여야 한다.
6. icon-only 버튼에는 반드시 `aria-label` 또는 `title`을 둔다.
7. 신규 화면의 접근성 체크: focus ring, keyboard path, contrast, target size, reduced motion.
8. `system.css`에는 화면 이름을 넣지 않는다. 반복되는 행은 `ds-action-row`, 반복 카드/표면은 `ds-card*`와 `ds-tone-*`로 조합한다.
9. 새 토큰은 세 화면 이상에서 반복될 때만 추가한다. 한 화면의 미세 조정 값은 화면 CSS에 둔다.

## 시각적 깊이 예산

한 화면에서 테두리·배경·그림자로 구획되는 깊이는 컨트롤까지 포함해 최대 3단계다.

1. **화면/영역** — 배경 또는 한 방향 divider만 사용한다. 둥근 사각형으로 감싸지 않는다.
2. **작업 표면/반복 항목** — 정보 그룹에 경계가 실제로 필요할 때 한 번만 사용한다.
3. **컨트롤/상태** — 버튼, 입력, 배지처럼 직접 조작하거나 상태를 읽는 요소다.

- `panel -> card -> card`, `inspector -> bordered list -> bordered row` 구조는 금지한다.
- 섹션 구분은 새 박스 대신 여백, 제목, 한 방향 divider를 우선한다.
- 카드 안에 버튼은 가능하지만, 그 버튼을 다시 카드나 패널로 감싸지 않는다.
- 새 grid track은 `1fr` 대신 `minmax(0, 1fr)`를 쓰고, 공통 layout 요소의 `min-width: 0` 규칙을 제거하지 않는다.
- 툴바·액션·페이지 이동·탭 행은 `ds-control-row`를 함께 쓴다. 화면별 높이는 `--ds-row-control-size`에 `--ds-control-*` 토큰을 지정한다.
- 버튼·입력·선택 컨트롤의 높이에 px 값을 직접 쓰지 않는다. 같은 행의 컨트롤은 하나의 높이 토큰을 공유한다.
- 정본 surface 이름과 다른 커스텀 경계를 만들면 JSX에 `data-ui-boundary`를 붙여 구조 검사에 포함한다.
- `pnpm check:ui`가 데스크톱과 워크스페이스 웹 전체에서 surface/control 중첩, control-row 누락, 안전하지 않은 fractional grid track, 하드코딩된 컨트롤 높이를 검사하며 `pnpm build`와 CI에서도 자동 실행된다.

## DataGrip 분석에서 반영한 것

- Database Explorer의 계층 구조와 view option 개념.
- Data editor의 toolbar + filter/sort strip + grid 구조.
- Query console의 editor/result/output/session 분리.
- 2026.1 MCP consent 모델의 네 가지 risk category.

반영하지 않은 것:

- 전체 IDE project/file/scratch/service 구조.
- 과도한 icon-only toolbar 밀도.
- safety를 Settings 내부에 숨기는 구조.
