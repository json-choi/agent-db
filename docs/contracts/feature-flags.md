# CLI·Terminal Platform feature flags

정본 이름은 `src-tauri/src/features.rs`다. 모든 flag는 기본 `off`다.

| Flag | 활성화 전 gate |
| --- | --- |
| `operation_runtime_v1` | migration/recovery/exact approval 검증 |
| `local_broker_v1` | peer identity, framing limit, stale discovery 검증 |
| `cli_v1` | protocol/secret snapshot/platform packaging 검증 |
| `skill_manager_v1` | atomic install과 user-modified 보존 검증 |
| `terminal_dock_v1` | CSP, PTY/process-tree/session revocation 검증 |
| `mcp_deprecated` | CLI parity와 config cleanup 준비 |
| `catalog_v2` | canonical Catalog V2 DTO를 CLI/ERD/DDL 소비자에 노출하기 전 engine fixture/fingerprint 검증 |
| `ddl_ir_v1` | engine renderer/fail-closed 검증 |
| `sql_documents_v1` | autosave/crash recovery 검증 |
| `table_changes_v1` | key/concurrency/exact proposal 검증 |
| `erd_v1` | Catalog V2/layout 성능 검증 |
| `jobs_v1` | checkpoint/file capability/bounded memory 검증 |
| `plugins_v1` | signature/capability/isolation 검증 |
| `workspace_resources_v1` | revision/conflict/RBAC 검증 |
| `realtime_collaboration_v1` | short-lived token/reconnect/compaction 검증 |

request field나 Agent/Plugin이 flag를 켤 수 없다. migration은 flag와 무관하게
idempotent하고 이전 binary가 모르는 새 table을 안전하게 무시할 수 있어야 한다.

현재 UI/MCP의 legacy `Catalog` wire를 보존하는 내부 `schema_cache_v2` adapter는
권한 scope와 cache CAS를 강화한 졸업된 persistence 기반이므로 이 flag로 되돌리지
않는다. 이 flag는 향후 canonical `CatalogSnapshot`을 새 CLI/ERD/DDL consumer에
노출하는 경로를 gate한다.
