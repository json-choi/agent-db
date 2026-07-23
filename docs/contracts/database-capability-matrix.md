# Database capability matrix

`supported`는 바로 실행한다는 뜻이 아니다. 모든 mutation은 DDL IR 검증, preview,
exact Operation Proposal, 승인, 실행 순서를 거친다. `blocked`는 raw SQL fallback을
만들지 않고 fail closed한다는 뜻이다.

| Capability | PostgreSQL | MySQL/MariaDB | SQLite | MongoDB |
| --- | --- | --- | --- | --- |
| Catalog namespace | supported | supported | synthetic `main`/attached | database/collection |
| SQL plan/run | supported | supported | supported | blocked |
| Typed document read | blocked | blocked | blocked | supported |
| DB-enforced read-only | transaction/session | transaction/session | query-only/read-only connection | typed stage allowlist + server role |
| Create/drop table | direct DDL | direct DDL | direct DDL | blocked in relational DDL IR |
| Rename table | direct DDL | direct DDL | direct DDL | blocked |
| Add column | direct DDL | direct DDL | capability/version checked | blocked |
| Alter/drop column | direct DDL | direct DDL | rebuild planner when required | blocked |
| PK/FK/unique/check | direct DDL | engine capability checked | rebuild planner | blocked |
| Expression/partial index | supported | capability checked | capability checked | blocked |
| Transactional DDL | engine/version dependent | implicit commit caveat | transaction where supported | blocked |
| Table row editor | stable key required | stable key required | stable key required | separate document editor later |
| Streaming export | planned | planned | planned | planned typed document export |
| Streaming import | planned | planned | planned | planned typed document import |

## DDL renderer 원칙

- identifier quoting은 engine adapter가 담당한다.
- PostgreSQL/MySQL/SQLite renderer가 지원하지 않는 IR은 error를 반환한다.
- SQLite rebuild는 새 table 생성, data copy, constraint/index 복원, rename의 전체 preview를
  만든다.
- MySQL implicit commit 가능성을 preview와 approval에 표시한다.
- MongoDB schema/index mutation은 relational DDL IR에 억지로 넣지 않고 별도 typed
  operation이 설계되기 전까지 차단한다.
