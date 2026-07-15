# Issue #29 작업 계획: 마이그레이션 기능 제거와 그룹 DB 비교 집중

- GitHub issue: <https://github.com/json-choi/dopedb/issues/29>
- 작업 브랜치: `work/json-choi/schema-group-diff-focus`
- 상태: 구현 완료 · 실제 PostgreSQL/MySQL/SQLite 연결 및 UI 수동 확인 대기

## 1. 목적

사용 빈도가 낮을 것으로 예상되는 폴더 기반 마이그레이션 기능을 제거한다. 기능 수를 유지하는 것보다 사용자가 자주 수행하는 작업을 더 쉽고 명확하게 만드는 데 집중하고, 같은 그룹으로 묶인 데이터베이스 사이의 실제 스키마 차이를 편리하게 확인할 수 있도록 한다.

이번 작업은 단순히 사이드바 메뉴 하나를 숨기는 작업이 아니다. 마이그레이션 기능이 차지하는 UI, 데이터 계약, IPC, 파일 감시, SQL 분석 및 실행 코드를 안전하게 걷어내고, 그 자리를 그룹 스키마 비교 흐름으로 재구성하는 제품 단순화 작업이다.

## 2. 사용자에게 보이는 결과

### 제거되는 경험

- 펼친 연결 아래의 `마이그레이션` 메뉴
- 마이그레이션 폴더 입력 및 선택
- 프로젝트 폴더에서 마이그레이션 폴더 자동 감지
- 마이그레이션 파일 감시와 자동 재분석
- 변경 기록, 생성된 rollback SQL, applied/pending 상태 표시
- 앱 안에서 마이그레이션 적용 및 롤백
- 연결 편집 화면의 마이그레이션용 `프로젝트 폴더`

### 강화되는 경험

- 스키마 그룹 헤더에서 비교 화면으로 바로 이동
- 그룹 안에서 기준 DB와 비교 대상을 명확하게 선택
- 그룹 구성원별 일치 여부와 `추가/누락/변경` 요약 확인
- 테이블, 컬럼, 인덱스, 외래 키 수준의 상세 diff 확인
- 검색과 상태 필터로 변경된 객체만 빠르게 탐색
- 그룹 전체 스키마를 한 번에 새로고침
- 일부 DB 연결이 실패해도 성공한 DB의 비교 결과는 계속 확인

## 3. 범위와 비범위

### 범위

- 폴더 기반 마이그레이션 프론트엔드 및 백엔드 제거
- 마이그레이션 전용 연결 데이터 제거
- 일반 SQL script 실행이 공유하던 statement splitter 분리
- 기존 스키마 그룹 데이터와 카탈로그 캐시를 활용한 비교 화면 추가
- 현재 diff 계산 모델 정리와 단위 테스트 추가

### 비범위

- SQL 탭의 단일 SQL 실행 변경
- SQL 탭의 다중 statement 실행 제거
- 외부 ORM 또는 마이그레이션 도구 연동
- DB 스키마를 자동으로 맞추는 DDL 생성 및 실행
- 서로 다른 엔진 사이의 스키마 변환
- 기존 사용자 `app.db`에서 `project_dir` 컬럼을 물리적으로 삭제하는 파괴적 마이그레이션

## 4. 설계 결정

### 4.1 사이드바는 요약, 메인 화면은 상세 비교를 담당한다

사이드바의 좁은 공간에는 그룹 상태와 비교 진입점만 둔다. 실제 diff는 메인 영역의 전용 `SchemaDiff` 화면에서 표시한다. 현재처럼 연결 트리 안에 누락 테이블과 변경 상태를 모두 섞어 표시하지 않는다.

### 4.2 기준 DB는 명시적으로 선택할 수 있다

- 그룹에 `prod` 환경 연결이 있으면 최초 기준으로 사용한다.
- `prod`가 없으면 그룹 정렬상 첫 연결을 사용한다.
- 사용자는 비교 화면에서 기준 DB를 바꿀 수 있다.
- 첫 구현에서는 그룹 키별 선택을 로컬에 기억한다.
- 별도 그룹 설정을 서버나 로컬 DB에 영구 저장해야 할 요구가 생길 때 first-class `schema_groups` 모델을 검토한다.

### 4.3 같은 엔진끼리 비교한다

첫 구현에서는 같은 엔진의 연결만 하나의 스키마 그룹으로 묶을 수 있게 한다. PostgreSQL과 MySQL처럼 엔진이 다른 DB는 타입 표현과 메타데이터 차이로 거짓 diff가 많아질 수 있으므로 비교 대상에서 제외한다.

### 4.4 기존 로컬 DB는 비파괴적으로 호환한다

새 설치용 스키마와 애플리케이션 데이터 계약에서는 `project_dir` 사용을 제거한다. 이미 만들어진 `app.db`의 컬럼은 남겨 두고 더 이상 읽거나 쓰지 않는다. SQLite 테이블 재작성까지 동반하는 물리적 컬럼 삭제는 하지 않는다.

### 4.5 일반 SQL script 실행은 보존한다

현재 `run_script`가 마이그레이션 모듈의 SQL statement splitter와 comment-only 판별 로직을 재사용한다. 마이그레이션 모듈 삭제 전에 이 로직을 중립적인 `sql_script` 모듈로 옮기고 기존 테스트를 함께 이전한다.

## 5. 목표 화면

### 그룹 헤더

- 그룹 이름과 엔진
- 그룹 전체 상태: `일치`, `변경 있음`, `확인 필요`
- `비교` 버튼
- 그룹 전체 새로고침 진입점

### 스키마 비교 화면

- 상단: 그룹 이름, 기준 DB 선택, 대상 DB 선택, 전체 새로고침
- 요약: 그룹 구성원별 `+ 추가`, `- 누락`, `~ 변경` 개수
- 필터: 전체, 추가, 누락, 변경
- 검색: 스키마, 테이블, 컬럼 이름
- 상세 목록: 객체 경로, 상태, 기준 값, 대상 값
- 테이블 확장: 컬럼, 인덱스, 외래 키 변경
- 연결별 독립적인 loading/error 상태

색상만으로 상태를 구분하지 않는다. `+`, `-`, `~` 기호와 상태 텍스트를 함께 사용한다.

## 6. 구현 순서

### 단계 1: 공용 SQL script 로직 분리

- [x] `src-tauri/src/sql_script/mod.rs`를 만든다.
- [x] 엔진별 statement splitting을 마이그레이션 모듈에서 옮긴다.
- [x] comment-only 또는 실행 효과가 없는 SQL 판별 로직을 옮긴다.
- [x] `commands::run_script`가 새 모듈을 사용하게 한다.
- [x] splitter 관련 테스트를 새 모듈로 이전한다.
- [x] 읽기 script, 쓰기 script, 주석, quoted semicolon, PostgreSQL dollar quote 회귀를 확인한다.

완료 기준: 마이그레이션 모듈을 참조하지 않고도 일반 다중 SQL 실행과 테스트가 통과한다.

### 단계 2: 마이그레이션 프론트엔드 제거

- [x] `src/screens/Migrations/`를 제거한다.
- [x] `App.tsx`의 `Migrations` import, `migrationsOpen` 상태 및 화면 분기를 제거한다.
- [x] `DatabaseExplorer`의 마이그레이션 메뉴와 관련 props를 제거한다.
- [x] `ConnectionForm`의 프로젝트 폴더 입력과 폴더 선택을 제거한다.
- [x] `ConnectionProfile.projectDir` TypeScript 필드를 제거한다.
- [x] 마이그레이션 및 프로젝트 폴더 i18n 키를 영어/한국어에서 제거한다.
- [x] 더 이상 사용하지 않는 마이그레이션 CSS와 공용 스타일을 제거한다.
- [x] `dopedb.migrationsDir.*` 로컬 저장값은 읽지 않게 한다. 별도의 사용자 데이터 삭제 코드는 추가하지 않는다.

완료 기준: 연결 트리와 연결 편집 화면 어디에도 폴더 기반 마이그레이션 기능이 노출되지 않는다.

### 단계 3: 마이그레이션 백엔드 제거

- [x] `analyze_migrations` 커맨드를 제거한다.
- [x] `detect_migrations_dir` 커맨드를 제거한다.
- [x] `start_migration_watch` 커맨드와 `migrations:changed` 이벤트를 제거한다.
- [x] `run_migration_script` 커맨드를 제거한다.
- [x] Tauri invoke handler 등록과 TypeScript IPC wrapper를 제거한다.
- [x] `MigrationReport`, `MigrationView` 등 전용 데이터 계약을 제거한다.
- [x] Rust `ConnectionProfile.project_dir`와 store의 insert/update/read 바인딩을 제거한다.
- [x] 신규 설치 스키마에서 `project_dir` 컬럼을 제거한다.
- [x] 마이그레이션 분석 및 applied-state 모듈을 제거한다.
- [x] 파일 감시에만 쓰이는 `notify` 의존성을 제거한다.
- [x] `pick_folder`가 더 이상 사용되지 않으면 커맨드와 wrapper를 제거한다. SQLite 파일 선택에 쓰이는 `pick_file`은 유지한다.
- [x] query history의 과거 `origin = migration` 값은 역사 데이터 호환을 위해 그대로 허용한다.

완료 기준: 일반 SQL 실행 이외의 코드에서 폴더 마이그레이션 개념과 의존성이 남지 않는다.

### 단계 4: 스키마 diff 도메인 정리

- [x] `schemaDiff.ts`의 결과를 화면 독립적인 object diff 형태로 정규화한다.
- [x] 테이블과 뷰의 추가/누락/변경을 구분한다.
- [x] 컬럼 이름, 타입, nullable, PK 변경의 이전 값과 이후 값을 보존한다.
- [x] 인덱스와 외래 키의 추가/누락/변경을 개별 항목으로 만든다.
- [x] summary count와 상세 목록이 같은 diff 결과를 사용하게 한다.
- [x] 비교 방향을 바꿀 때 `added`와 `missing`이 정확하게 반전되는지 테스트한다.
- [x] 대소문자가 다른 그룹명과 그룹 정렬을 테스트한다.
- [x] 서로 다른 엔진의 그룹화를 차단하는 규칙을 추가한다.

예상 모델:

```ts
type SchemaDiffStatus = "added" | "missing" | "changed" | "same";

interface SchemaObjectDiff {
  objectType: "table" | "view" | "column" | "index" | "foreignKey";
  path: string;
  status: SchemaDiffStatus;
  baselineValue?: string;
  targetValue?: string;
}
```

완료 기준: 사이드바 요약과 메인 비교 화면이 하나의 검증된 diff 결과를 공유한다.

### 단계 5: 그룹 비교 화면 구현

- [x] `src/screens/SchemaDiff/index.tsx`와 화면 전용 CSS를 추가한다.
- [x] `DatabaseExplorer` 그룹 헤더에 비교 버튼과 상태 요약을 추가한다.
- [x] App에 선택된 그룹을 여는 화면 상태를 추가한다.
- [x] `catalogQuery`와 `useQueries`로 그룹 구성원의 카탈로그를 병렬 로드한다.
- [x] 그룹을 펼쳤는지와 무관하게 비교 화면이 필요한 카탈로그를 로드하게 한다.
- [x] 기준 DB와 대상 DB 선택 UI를 추가한다.
- [x] 그룹 구성원별 요약과 상세 diff 목록을 추가한다.
- [x] 상태 필터와 객체 이름 검색을 추가한다.
- [x] 그룹 전체 새로고침으로 각 카탈로그를 live introspection하고 공유 캐시에 반영한다.
- [x] 연결별 loading/error를 독립적으로 표시한다.
- [x] 그룹 멤버가 1개뿐일 때 비교할 연결이 필요하다는 empty state를 표시한다.

완료 기준: 사용자가 그룹 헤더에서 한 번의 동작으로 비교 화면을 열고, 어떤 DB에 무엇이 다른지 상세 수준까지 확인할 수 있다.

### 단계 6: 검증과 정리

- [x] `pnpm build`
- [x] TypeScript diff 단위 테스트
- [x] `cargo test --manifest-path src-tauri/Cargo.toml`
- [x] `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`
- [ ] PostgreSQL/MySQL/SQLite 동일 엔진 그룹별 수동 확인
- [x] 기존 `project_dir` 컬럼을 가진 연결 row가 데이터 손실 없이 왕복되는지 자동 테스트
- [x] 일반 다중 SQL script의 읽기/쓰기/승인 흐름 회귀 확인
- [ ] 영어/한국어 UI와 좁은 사이드바 레이아웃 확인

## 7. 주요 파일 지도

| 영역 | 현재 파일 | 계획 |
| --- | --- | --- |
| 마이그레이션 화면 | `src/screens/Migrations/` | 제거 |
| 앱 화면 전환 | `src/App.tsx` | migration 상태 제거, group diff 상태 추가 |
| 연결 트리 | `src/screens/Connections/DatabaseExplorer.tsx` | migration 행 제거, 그룹 비교 진입점 추가 |
| 연결 폼 | `src/screens/Connections/ConnectionForm.tsx` | 프로젝트 폴더 제거 |
| diff 로직 | `src/lib/schemaDiff.ts` | 상세 object diff 모델로 확장 |
| 읽기 캐시 | `src/lib/queries.ts` | 기존 catalog query 재사용 |
| TS IPC | `src/ipc/commands.ts`, `src/ipc/types.ts` | migration API와 타입 제거 |
| Rust 명령 | `src-tauri/src/commands/mod.rs` | migration 명령 제거, run_script 참조 변경 |
| Rust 모듈 | `src-tauri/src/migrations/` | splitter 분리 후 제거 |
| Rust 계약 | `src-tauri/src/model.rs` | project_dir 제거 |
| 로컬 저장소 | `src-tauri/src/store/` | 신규 project_dir 사용 제거, 구 DB 호환 유지 |
| 의존성 | `src-tauri/Cargo.toml` | notify 제거 여부 확인 |
| 번역 | `src/lib/i18n.tsx` | migration 키 제거, schema diff 키 추가 |

## 8. 위험과 대응

### 다중 SQL 실행 회귀

가장 큰 기술적 위험이다. 마이그레이션 모듈보다 먼저 splitter를 분리하고 테스트를 통과시킨 뒤 제거 작업을 시작한다.

### 오래된 `app.db` 호환성

기존 `project_dir` 컬럼은 남겨 두고 무시한다. 연결 row 역직렬화와 upsert SQL의 positional bind 변경을 함께 검증한다.

### 캐시된 스키마가 실제 DB와 다른 문제

기본 화면은 빠른 표시를 위해 캐시를 사용할 수 있지만, `그룹 전체 새로고침`은 live introspection 결과를 query cache에 기록한다. 화면에 마지막 갱신 상태를 명확하게 표시한다.

### 그룹 크기에 따른 연결 부하

그룹 구성원은 병렬로 읽되 무제한 동시 실행을 만들지 않는다. 일반적인 dev/staging/prod 3개 연결을 우선 대상으로 하고, 향후 큰 그룹이 등장하면 동시성 제한을 추가한다.

### diff 정확도 범위

첫 버전은 현재 catalog가 제공하는 테이블, 뷰, 컬럼, 인덱스, 외래 키를 기준으로 한다. default, check constraint, identity, view definition 비교는 introspection 계약 확장이 필요하므로 후속 범위로 둔다.

## 9. 최종 인수 조건

- [x] 캡처에서 지목된 연결 하위 `마이그레이션` 항목이 사라진다.
- [x] 프로젝트 또는 마이그레이션 폴더를 지정하는 UI가 사라진다.
- [x] 폴더 분석, 감시, 적용, 롤백 코드와 전용 의존성이 제거된다.
- [x] 일반 SQL과 다중 statement 실행은 이전과 동일하게 동작한다.
- [x] 그룹 헤더에서 스키마 비교 화면을 열 수 있다.
- [x] 기준 DB를 선택하고 하나 또는 모든 그룹 구성원과 비교할 수 있다.
- [x] 추가, 누락, 변경 사항을 요약과 상세 수준에서 확인할 수 있다.
- [x] 새로고침과 부분 실패 상태를 사용자가 이해할 수 있다.
- [x] 기존 사용자 연결 데이터가 손실되지 않는다.
- [x] 빌드, Rust 테스트, lint와 diff 단위 테스트가 통과한다.
