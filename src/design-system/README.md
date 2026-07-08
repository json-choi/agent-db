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

## 컬러 팔레트

아이콘에서 추출한 녹색을 primary hue로 삼고, chromatic color는 세 가지로 제한한다. 중립, 텍스트, border, glass alpha 값은 제외한다.

- Primary Green: 브랜드, 선택 상태, focus, success, info, trust.
- Caution Amber: review, warning, medium risk.
- Critical Coral: destructive, error, blocked, high risk.

새 UI에서 파랑, 청록, 보라 등 별도 hue를 추가하지 않는다. 기존 의미 토큰은 유지하되 `--ds-accent`, `--ds-info`, `--ds-success`, `--ds-source-blue`, `--ds-source-teal`, `--ds-source-green`은 모두 primary green으로 alias한다. 경고는 `--ds-caution`, 위험은 `--ds-critical` 계열로만 표현한다.
Primary green은 배경 fill에는 `--ds-accent`, 링크/아이콘/강조 텍스트에는 `--ds-accent-text`를 사용한다.

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
- 새 화면은 `ds-panel`, `ds-card`, `ds-toolbar`, `ds-filter-strip`, `ds-grid-shell` 같은 공통 클래스를 먼저 사용한다.
- 불투명 배경색이 필요하면 하드코딩하지 말고 `tokens.css`에 의미 기반 `--ds-*` 토큰을 추가한다.

## 공통 클래스

Layout:

- `ds-stack`, `ds-stack-tight`
- `ds-inline`, `ds-inline-wrap`
- `ds-page-head`
- `ds-workbench-head`
- `ds-workbench-title`
- `ds-title-line`
- `ds-meta-row`, `ds-meta-dot`
- `ds-command-group`
- `ds-panel`, `ds-card`, `ds-inspector`

Controls:

- `ds-button`
- `ds-button-primary`
- `ds-button-danger`
- `ds-button-small`
- `ds-icon-button`
- `ds-input`

Status:

- `ds-badge`
- `ds-status-ok`
- `ds-status-warning`
- `ds-status-danger`
- `ds-context-badge`

Database workflow:

- `ds-toolbar`
- `ds-data-toolbar`
- `ds-toolbar-group`
- `ds-toolbar-spacer`
- `ds-filter-strip`
- `ds-filter-token`
- `ds-object-row`
- `ds-count`
- `ds-grid-shell`

Agent/safety workflow:

- `ds-trust-surface`
- `ds-risk-surface`
- `ds-danger-surface`
- `ds-ledger-grid`
- `ds-ledger-card`
- `ds-ledger-card-header`
- `ds-ledger-card-trust`
- `ds-ledger-card-risk`
- `ds-ledger-card-danger`
- `ds-policy-grid`
- `ds-policy-card`
- `ds-policy-card-trust`
- `ds-policy-card-risk`
- `ds-policy-card-danger`

## 사용 예시

```tsx
<section className="ds-panel ds-stack">
  <header className="ds-page-head">
    <div>
      <h2>Agent trust ledger</h2>
      <p className="muted">What the agent can see and what needs approval.</p>
    </div>
    <span className="ds-context-badge">prod.public</span>
  </header>

  <div className="ds-policy-grid">
    <article className="ds-policy-card">
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

## DataGrip 분석에서 반영한 것

- Database Explorer의 계층 구조와 view option 개념.
- Data editor의 toolbar + filter/sort strip + grid 구조.
- Query console의 editor/result/output/session 분리.
- 2026.1 MCP consent 모델의 네 가지 risk category.

반영하지 않은 것:

- 전체 IDE project/file/scratch/service 구조.
- 과도한 icon-only toolbar 밀도.
- safety를 Settings 내부에 숨기는 구조.
