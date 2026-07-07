# dopedb — Architecture

> Status: authoritative design doc (ARCHITECTURE.md). Date: 2026-07-01. Author: lead architect.
> Scope: MVP. Confirmed decisions from the project brief are treated as fixed and designed around, not re-argued.

---

## 1. Product summary & core concept

dopedb is a macOS-first, genuinely native desktop database client whose differentiator is that it drives an AI agent to operate databases in natural language **without ever billing its own LLM API** — it shells out to the CLI the user already subscribes to (Anthropic's Claude Code or OpenAI's codex, authed via their own OAuth subscription) as a non-interactive backend subprocess. The agent is a reasoning brain that *proposes* SQL grounded in live schema; dopedb owns the connection, credentials, and a non-negotiable safety pipeline (read-only by default, human approval gate, full audit log, and writes wrapped in a transaction with EXPLAIN/rollback impact preview). The agent never touches the database directly — every statement it emits re-enters dopedb's own safety gate as untrusted input.

---

## 2. High-level architecture

```
┌──────────────────────────────────────────────────────────────────────────┐
│  FRONTEND  (WKWebView)  — React + TS + Vite                                │
│  • Connection manager UI   • CodeMirror 6 SQL editor + schema autocomplete │
│  • TanStack results grid (virtualized, inline edit)   • ⌘K palette         │
│  • Approval card (SQL + plain-English restatement + risk badge + N rows)   │
│  • Safe-Mode diff view for staged writes                                   │
└───────────────▲───────────────────────────────────────┬───────────────────┘
                │  Tauri IPC (#[command], typed AppError)  │  ipc::Channel<T>
                │  request/response                        │  streaming rows + agent events
┌───────────────┴───────────────────────────────────────▼───────────────────┐
│  RUST CORE  (Tauri v2, tokio)                                              │
│                                                                            │
│  ┌───────────────┐  ┌──────────────┐  ┌───────────────┐  ┌──────────────┐  │
│  │ Connection    │  │ Query        │  │ Safety Engine │  │ Agent Bridge │  │
│  │ Manager       │─▶│ Executor     │◀▶│ L1 parse/     │  │ AgentBackend │  │
│  │ Pool per conn │  │ paginate,    │  │   classify    │  │ trait:       │  │
│  │ (Pg/My/Sqlite)│  │ cancel,      │  │ L2 ro-role/txn│  │ Claude|Codex │  │
│  │ + SSH tunnels │  │ EXPLAIN,     │  │ L3 dry-run    │  │ subprocess   │  │
│  │               │  │ tx+rollback  │  │ L4 approval   │  │ + MCP server │  │
│  └───────┬───────┘  └──────┬───────┘  └───────┬───────┘  └──────┬───────┘  │
│          │                 │                  │                 │          │
│  ┌───────┴─────────────────┴──────────────────┴─────────────────┴──────┐   │
│  │ Audit (append-only hash-chained SQLite) + Schema introspection cache │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└──────────┬────────────────────────────────────────────────────┬────────────┘
           │                                                     │
   ┌───────▼────────┐                                   ┌────────▼──────────┐
   │  TARGET DBs    │                                   │  SUBSCRIBED CLI   │
   │  Postgres      │                                   │  claude -p / codex│
   │  MySQL/Maria   │                                   │  exec (spawned    │
   │  SQLite        │◀── read-only MCP introspection ───│  subprocess, OAuth│
   │  Supabase/Neon │    tools (get_schema/explain)     │  from ~/.claude,  │
   │  PlanetScale   │                                   │  ~/.codex)        │
   └────────────────┘                                   └───────────────────┘
```

Flow of one NL request: Frontend → `ask_agent(conn_id, prompt)` command → Agent Bridge builds redacted schema context → spawns CLI → parses `{sql, rationale, is_write}` → **hands SQL to Safety Engine, never to the DB** → L1 parse/classify → L3 dry-run preview → L4 approval card to frontend → on approve, Query Executor runs (read-only or tx-wrapped) → Audit records every transition → rows stream back over `Channel`.

---

## 3. The agent bridge (the heart)

### 3.1 Decisions

| Question | Decision |
|---|---|
| Default CLI | **Claude Code** (`claude -p`). Single terminal `result` JSON object, `session_id` resume, mature `--permission-mode`/`--mcp-config`. |
| Alternative | **codex** (`codex exec --json`) — first-class, behind the same trait; cleaner event JSONL + native `--output-schema`. User picks whichever they subscribe to. |
| Spawn mechanism | `tokio::process::Command` (**not** a Tauri sidecar/`externalBin` — these are user-installed binaries). |
| SQL generation transport | **Shell-out** for the generation turn. |
| Grounding transport | **MCP stdio server** exposed *by dopedb*, read-only introspection tools only. |
| What the agent produces | A JSON object `{sql, rationale, is_write}`. Nothing is executed agent-side. |

### 3.2 Normalized abstraction

```rust
trait AgentBackend {
    async fn run(&self, req: SqlRequest, tx: Sender<AgentEvent>) -> Result<Final>;
}
enum AgentEvent { Reasoning(String), AssistantText(String), ToolCall{name:String}, Usage(TokenUsage), Error(String) }
struct Final { sql: String, rationale: String, is_write: bool }  // is_write is a HINT, never trusted for safety
```

**CodexBackend:**
`codex exec --json --skip-git-repo-check --ephemeral -s read-only -m <model> --output-schema sql.json -`, prompt on **stdin**. Map `agent_message`→`AssistantText`, `reasoning`→`Reasoning`, `turn.completed.usage`→`Usage`. Use `--output-schema` for a strict `{sql,rationale,is_write}` final object.

**ClaudeBackend (default):**
`claude -p --output-format stream-json --verbose --permission-mode plan --append-system-prompt "<force strict JSON {sql,rationale,is_write}, no code fences>" --model sonnet`, prompt on stdin. No native output schema → pin structure via system prompt **and regex-strip ` ```sql ` fences defensively** (observed: Claude wraps SQL in a fence even when told not to). `--session-id` enables multi-turn refine.

### 3.3 Spawn hardening (both backends)

- Resolve the binary via a **login-shell PATH probe** (GUI apps get a minimal PATH) with a user-overridable explicit path; fail loudly, never silently.
- Inherit `HOME`/`PATH` so the child reads `~/.codex` / `~/.claude` OAuth creds + keychain.
- Put the child in its **own process group** (`command_group` crate) so cancel kills the whole tree; wrap every run in `tokio::time::timeout` with a hard `SIGKILL` on the group.
- Parse stdout **line-by-line** with `serde_json` over `tokio::io::Lines`; tolerate unknown event types; take the **last** `agent_message`/`result` as authoritative.
- **Preflight** at startup: `--version` (feature-gate by version) and a cheap auth check (`codex login status` / a trivial `claude -p`) so a TTY-less token-refresh hang is caught early, not mid-query.

### 3.4 Schema context (grounding)

Two channels, both read-only:

1. **In-prompt context (default, always):** dopedb's own introspection (§5.4) serialized to a compact DDL/JSON summary — tables → columns/types/PK/FK + row-count estimates. **Relevance-filtered**, not a full dump (large schemas blow context). Cached in `schema_cache`. Passed on **stdin**, never argv (avoids process-list leakage).
2. **Live MCP tools (opt-in, for iterative grounding):** dopedb runs as an MCP stdio server exposing **read-only tools only** — `list_schemas`, `get_object_details`, `sample_rows`, `explain_query`. Tool shape mirrors crystaldba/postgres-mcp so agent mental models transfer. Attached via `codex mcp add` / `claude --mcp-config`.

**Hard rule: no `execute_sql` write tool over MCP.** The agent only *proposes*; execution stays in dopedb's UI behind the approval gate. A fully DB-connected MCP write tool would bypass the entire safety model — it does not exist in dopedb.

> **Known bug guard (codex #15451):** `--json`/`--output-schema` are silently ignored when MCP tools are attached. Therefore the **strict-schema SQL-generation turn runs WITHOUT MCP tools**; MCP grounding, when enabled, is a *separate* preceding exploration turn. Never combine `--output-schema` with attached MCP tools.

### 3.5 Data protection to the CLI

Send **schema DDL only, never row data** by default. Redact in Rust before spawn: strip sample values, mask column names matching PII patterns (email/ssn/token) unless the user opts in per connection. Treat all CLI output as **untrusted** → it re-enters L1–L4. Document plainly: schema names/comments leave the machine and vendors may log prompts server-side.

---

## 4. Safety architecture

**Governing principle:** L1 parsing is a UX / early-reject filter. **The authoritative security boundary is L2 (the database's own session/role capability).** A parser cannot see through functions, procedures, extensions, writable CTEs, or dialect quirks; the model can craft SQL that parses as a read but writes. **Rust enforces policy; the DB enforces capability.**

### 4.1 Threat → authoritative stopping layer

| Threat | L1 parse (Rust) | L2 session/role (DB) — authoritative | L3 tx+preview | L4 human gate |
|---|---|---|---|---|
| Write emitted in read-only mode | classify + reject | **read-only txn/role rejects** | — | — |
| `DELETE`/`UPDATE` without `WHERE` | detect, flag high-risk | — | rollback shows N rows | **approve** |
| Multi-statement / stacked (`;`) | **reject if >1 statement** | single-stmt driver guard | — | — |
| `DROP`/`ALTER`/`GRANT`/DDL | classify | **least-priv role denies** | — | approve |
| Runaway/expensive query | timeout guess | **`statement_timeout`** | EXPLAIN cost | — |
| Function/writable-CTE writes (`lo_export`, `WITH … DELETE`) | best-effort reclassify | **read-only txn is authoritative** | — | — |
| Exfiltration to the CLI | **redaction** | — | — | — |

### 4.2 L1 — parse & classify

- Parser: **`sqlparser-rs` for MySQL/SQLite**, **`pg_query.rs` (libpg_query bindings) for PostgreSQL.** Rationale: sqlparser's Postgres dialect gaps risk read-only-bypass false-negatives (writable CTEs, `COPY … TO PROGRAM`, side-effecting functions); libpg_query is the real PG grammar. This is a deliberate exception to "one parser" because a false-negative here is a data-loss bug. (Because L2 is authoritative anyway, L1 gaps degrade UX, not safety — but for the engine we ship most of, use the real grammar.)
- **Reject if statement count > 1** (kills stacked injection regardless of engine).
- Classify: `Query`→read; `Insert/Update/Delete/Merge`→write; `Drop/Alter/Truncate/Create`→DDL; `Grant/Revoke/SetRole`→privilege. Walk `Update`/`Delete`; `selection.is_none()` → `NO_WHERE` high-risk.
- **Recurse into `Query` bodies for DML CTEs and reclassify as write.** Treat any parse ambiguity or parse error as write / reject-to-DB.

### 4.3 L2 — DB-level enforcement (authoritative, per engine)

- **PostgreSQL:** dedicated **least-privilege login role** (only `SELECT` + `USAGE`) where the user can create one; per-request `BEGIN; SET TRANSACTION READ ONLY; SET LOCAL statement_timeout='15s';`. `READ ONLY` blocks DML/DDL **and writable CTEs** at execute time (error `25006`). `PgPool` `after_connect` sets `default_transaction_read_only=on` + `SET ROLE`.
- **MySQL/MariaDB:** `START TRANSACTION READ ONLY`; timeout via `SET SESSION max_execution_time`/`MAX_EXECUTION_TIME` hint. Caveat: `max_execution_time` only applies to `SELECT`, so writes rely on the read-only txn + a `SELECT`-only `GRANT`.
- **SQLite:** open a second connection `SQLITE_OPEN_READONLY` (`SqliteConnectOptions::read_only(true)`) / `PRAGMA query_only=ON` — file-level, unforgeable. No server timeout → wall-clock `tokio::time::timeout` + `sqlite3_interrupt`.

### 4.4 L3 — dry-run / impact preview

- **Reads:** `EXPLAIN (FORMAT JSON)` (pg) / `EXPLAIN FORMAT=JSON` (mysql) / `EXPLAIN QUERY PLAN` (sqlite). Parse row/cost estimate for the approval card. **Never `EXPLAIN ANALYZE` a write** — it executes.
- **Writes (exact N):** open explicit txn → execute the real statement → capture `rows_affected()` → **unconditional `ROLLBACK`**. Gives true N vs. estimate. **Flag statements referencing functions/`RETURNING`** — triggers with external effects (NOTIFY, dblink) fire before rollback.
- **PlanetScale caveat:** its `EXPLAIN` is Vitess-planned, not native MySQL — mark write-impact estimates there as lower-confidence in the UI.

### 4.5 L4 — human approval gate

Approval card renders: statement class + risk badge, dialect, full SQL, **plain-English restatement**, parsed target tables/columns, preview N (from L3 rollback), `NO_WHERE` warning, timeout. Read-only SELECTs may auto-run; **writes/DDL are hard-gated** and require explicit confirm. Staged writes surface as a TablePlus-style **Safe-Mode diff** before COMMIT. In read-only mode, writes are blocked outright.

### 4.6 Audit log

Every attempt → append-only `audit_log`: ts, conn id, engine, raw NL prompt, generated SQL, class, approve/reject + user, exec result/error, rows affected, duration, `agent_cli`. **Hash-chain** each row (`hash = SHA256(prev_hash ‖ canonical_row)`) for tamper-evidence. Stated limit: detects post-hoc edits, not cryptographically strong against a local attacker with file access. `audit_log` (compliance) is kept separate from `query_history` (UX/replay).

---

## 5. Data & connectivity

### 5.1 Driver layer

**`sqlx` 0.8.6** (pin; 0.9 emerging) — one async API for Pg/MySQL/SQLite via features. Engine-specific `PgPool`/`MySqlPool`/`SqlitePool` for user DBs (proper EXPLAIN/introspection); `AnyPool` only for dopedb's own local store.

- Features: `runtime-tokio`, `tls-rustls-ring`, `postgres`, `mysql`, `sqlite` (bundled), `chrono`/`uuid`/`json`.
- **TLS pick: `tls-rustls-ring`** (resolving the two reports' conflict — `ring` over `aws-lc-rs`): fewer cross-arch build surprises on universal-binary builds; both avoid OpenSSL linking pain, and we don't need aws-lc-rs FIPS. Revisit only if a provider needs a cipher `ring` lacks.
- **No compile-time `query!` macros** — this is a runtime-arbitrary-SQL client. Use `sqlx::query` + dynamic `Column`/`TypeInfo` column reads.
- Custom-CA field + bundle AWS RDS global CA + ISRG roots (`.ssl_root_cert(...)`) — rustls rejects custom-CA RDS certs otherwise.

### 5.2 Per-provider connection handling

| Provider | Host/port | TLS | Key gotcha |
|---|---|---|---|
| Supabase pooler (Supavisor) | `…pooler.supabase.com` — **6543 txn**, 5432 session | require | user is `postgres.<ref>`; txn mode → **`statement_cache_capacity(0)`** or connections break. Direct `db.<ref>` is IPv6-only → default to pooler. |
| Neon | `ep-…-pooler.<region>.neon.tech` | require + `channel_binding=require` | scale-to-zero cold start → generous connect timeout; drop `-pooler` for DDL; branch = distinct host. |
| PlanetScale MySQL | `…connect.psdb.cloud` | require, verify-identity | Vitess: FK metadata unreliable (sharded); no cross-db `USE`; some `information_schema` differs. |
| PlanetScale Postgres | provider host + PgBouncer | require | real PG18 → treat like Neon. |
| RDS / self-hosted PG | host:5432 | verify-full + RDS CA | SSH tunnel for private/VPC. |
| RDS / self-hosted MySQL/Maria | host:3306 | REQUIRED | same driver. |
| SQLite | file path | n/a | store absolute path; default `mode=ro`. |

**SSH tunnels: `russh` 0.5x** (thrussh is dead) — local `TcpListener` → `channel_open_direct_tcpip` → point sqlx at `127.0.0.1:<port>`; key + agent auth. Reserved for bastion/VPC topologies (the four cloud providers terminate TLS publicly). Flagged as the **highest-risk custom component** — budget for reconnect/keepalive.

### 5.3 Local SQLite data model

At `~/Library/Application Support/dopedb/app.db` (`app_data_dir`), `SqlitePool`, `journal_mode=WAL`, `foreign_keys=ON`. **Secrets never live here.**

```
connections(id PK, name, engine, host, port, db_name, username, sslmode,
  ssh_config JSON NULL, extra_params JSON, secret_ref TEXT,  -- keychain item id, NOT the password
  readonly_default INT DEFAULT 1, created_at, updated_at)

connection_safety(connection_id FK, require_approval INT DEFAULT 1, allow_writes INT DEFAULT 0,
  wrap_writes_in_tx INT DEFAULT 1, explain_preview INT DEFAULT 1, max_rows INT)

query_history(id PK, connection_id FK, sql, kind, status, row_count, duration_ms, error,
  executed_at, origin)                         -- agent|manual  (UX/replay)

audit_log(id PK, connection_id FK, sql, action, agent_cli, agent_prompt, approved_by,
  affected_estimate INT, ts, prev_hash TEXT)   -- append-only, hash-chained (compliance)

snippets(id PK, connection_id FK NULL, title, sql, tags JSON, updated_at)
schema_cache(connection_id FK, introspected_at, catalog_json TEXT)
```

### 5.4 Keychain & introspection

- **Secrets → macOS Keychain** via **`keyring-core` + `apple-native-keyring-store`** (the 2026 restructuring of `keyring` v4; starting fresh, target these, not legacy v3). Service = bundle id, account = conn id. Store only the password/connection-string secret; the DB holds a `secret_ref`. **Pitfall:** unsigned/ad-hoc builds hit `errSecMissingEntitlement (-34018)` — Keychain only works in properly code-signed builds; test it only there.
- **Introspection** on first connect + on-demand, cached in `schema_cache`: PG `pg_catalog`+`information_schema` (+ `pg_constraint`, `pg_type`, `pg_description`); MySQL `information_schema.*` (ignore FK rows on PlanetScale); SQLite `PRAGMA table_info/foreign_key_list/index_list` + `sqlite_master.sql`. Serialize to token-bounded summary with **relevance filtering**.

---

## 6. Tech stack decisions

| Layer | Pick | One-line rationale |
|---|---|---|
| App shell | **Tauri v2** (Rust + WKWebView) | Native macOS feel, Rust core for the safety engine, small bundle. |
| DB drivers | **sqlx 0.8.6** (`tls-rustls-ring`, pg/mysql/sqlite bundled) | One async API + pooling for all MVP targets incl. cloud connection-string providers. |
| SQL parse (PG) | **pg_query.rs** (libpg_query) | Real Postgres grammar — closes read-only-bypass false-negatives for our primary engine. |
| SQL parse (MySQL/SQLite) | **sqlparser-rs 0.61** | Good enough where L2 is authoritative and no libpg_query equivalent exists. |
| SSH tunnel | **russh 0.5x** (+ russh-keys) | Only maintained pure-Rust SSH; thrussh is dead. |
| Subprocess | **tokio::process** + **command_group** | User-installed CLIs need real spawn + process-group kill, not sidecar bundling. |
| Agent default | **Claude Code** (`claude -p`); codex alt | Cleanest single-object JSON + session resume; both behind `AgentBackend` trait. |
| Secrets | **keyring-core + apple-native-keyring-store** | Native Keychain, current (2026) crate structure. |
| Cancellation | **tokio_util CancellationToken** + PG driver cancel | Client- and server-side query cancel. |
| Frontend | **React + TS + Vite** | Largest grid/editor ecosystem; native-feel achievable via Tauri chrome. |
| Results grid | **TanStack Table + Virtual**, Glide only at 100k+ rows | Headless + virtualized covers typical sets; escalate to canvas only when needed. |
| SQL editor | **CodeMirror 6** (`lang-sql`) | Lighter than Monaco, easy schema-driven autocomplete, better webview fit. |
| State/data | **TanStack Query** | Command caching + invalidation for IPC calls. |
| Streaming | **tauri::ipc::Channel<T>** | Incremental row batches + agent events without giant IPC payloads. |
| Packaging | **Developer ID, hardened runtime, NO App Sandbox, off the Mac App Store** | Sandbox forbids exec of external `codex`/`claude` — this is a fixed constraint. |
| Entitlements | `com.apple.security.cs.allow-jit` (+ notarize via env creds) | Missing it = clean dev build, crashing notarized build. |
| Auto-update | **@tauri-apps/plugin-updater** + signed `latest.json` | Standard signed update channel. |

**Fixed packaging constraint (call-out):** the core feature spawns an externally-installed CLI at an arbitrary PATH. **App Sandbox forbids fork/exec outside the bundle → MAS distribution is incompatible.** Ship Developer ID direct/Homebrew, hardened runtime, no sandbox. Non-negotiable.

---

## 7. Key risks & open questions (ranked)

1. **ToS / subscription-as-backend (highest).** OpenAI (Apr 2026) steers programmatic Codex toward API-key billing and enforces token-credit limits on a **5-hour rolling window**; Anthropic similar. Driving a subscription OAuth session as a server backend risks limit exhaustion or ToS friction. **Mitigation:** frame dopedb as orchestrating *the user's own interactively-authed CLI* — one turn per explicit user action, never batch automation; surface `429`/rate-limit errors verbatim; offer an **optional API-key path** per backend. *Open: exact per-plan rate-limit surfacing in `exec --json` errors.*
2. **Unstable / undocumented CLI output contract.** JSON shapes are unversioned; codex #15451 (schema silently dropped under MCP), Claude fence-wrapping. **Mitigation:** detect version at startup, feature-flag by version, tolerant last-message parser, snapshot tests pinned per version, run the strict-schema turn without MCP. *Open: whether `--output-schema` strictness holds across future codex releases.*
3. **L1 read-only-bypass false-negatives.** Writable CTEs / side-effecting functions can parse as reads. **Mitigation:** L2 (read-only txn/role) is authoritative and closes this; libpg_query for PG narrows L1; CTE recursion + ambiguity-as-write. Residual: MySQL/SQLite rely more on L2 + sqlparser.
4. **Auth/session inheritance & headless hangs.** Child depends on `~/.codex`/`~/.claude` OAuth + Keychain; token refresh or re-login prompt can hang a TTY-less child. **Mitigation:** startup preflight auth check, inherited env + explicit `HOME`, hard timeouts + process-group kill, detect auth-error strings → prompt re-login.
5. **SSH tunneling robustness.** Highest-risk custom component (reconnect, keepalive, key/agent auth). **Mitigation:** scope to bastion/VPC only for MVP; budget explicit time; consider deferring to fast-follow if it threatens the schedule.
6. **Keychain requires signed builds.** `-34018` on unsigned/ad-hoc builds. **Mitigation:** wire signing early; provide a dev fallback (encrypted file) gated to debug builds only.
7. **PlanetScale/Vitess divergence.** FK metadata unreliable (sharded), `EXPLAIN` is Vitess-planned so write-impact preview is lower-confidence, some `information_schema` differs. **Mitigation:** don't trust FK introspection there; mark PS write previews lower-confidence in the UI; test the agent against PS specifically.
8. **Schema context vs. token budget.** Large schemas blow agent context. **Mitigation:** relevance-filter to referenced/likely tables + cache snapshots; MCP `get_object_details` for on-demand drill-in instead of dumping everything.

**Open questions to resolve before/early in build:** (a) do we require users to create a least-priv PG role, or auto-generate one on first connect? (b) default: auto-run read-only SELECTs, or gate everything until the user opts into auto-run? (c) MCP grounding on by default or opt-in per connection given the codex #15451 interaction?

---

Decisions I made where reports disagreed: TLS backend → `tls-rustls-ring` (over `aws-lc-rs`); PG parser → libpg_query/`pg_query.rs` (over sqlparser-only); Keychain crate → `keyring-core`+`apple-native-keyring-store` (over legacy v3); default agent → Claude Code (codex first-class alt). Everything else was consistent across reports.