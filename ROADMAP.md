# ROADMAP.md — agent-db

> Companion to ARCHITECTURE.md (2026-07-01). Phased build plan for a small team (2–3 engineers). Sequencing rule enforced throughout: **every safety primitive that gates writes ships before the write path it gates.** Stretch items are marked `⟂ stretch` and can slip without blocking the phase.

---

## Sequencing principle

The read path and the four safety features are built **before** any statement that can mutate data is executable. Concretely: read-only enforcement (L2) and the audit log land in Phase 1; the human approval gate and transaction/rollback preview land in Phase 2; **writes are not enabled at all until Phase 3**, on top of all four. There is no phase in which the agent can write without approval + audit + tx-wrap present.

| Safety feature | Where implemented | Lands in |
|---|---|---|
| **(1) Read-only by default** | `safety::l2_enforce` (per-engine read-only txn / role / `SQLITE_OPEN_READONLY`) + `connection_safety.readonly_default=1` | **Phase 1** |
| **(3) Audit log** | `audit` module (append-only hash-chained SQLite `audit_log`) | **Phase 1** |
| **(2) Human approval gate** | `safety::l4_gate` + frontend Approval Card | **Phase 2** |
| **(4) Tx-wrapped writes + rollback/EXPLAIN preview** | `safety::l3_preview` (EXPLAIN for reads, execute+`ROLLBACK` for exact N) + `executor::tx` | **Phase 2** (preview) → armed for real writes in **Phase 3** |

L1 parse/classify is a UX pre-filter and ships in Phase 1 alongside L2; it is not the security boundary.

---

## Phase 0 — De-risking spike (PROVE the CLI backend)

**Goal:** prove, with the smallest possible code, that a subscribed CLI can be driven non-interactively to return a SQL statement that agent-db then runs **read-only** against a real database. No UI, no Tauri, no persistence. A throwaway Rust binary (`cargo run`). If this doesn't hold, the whole product thesis is wrong — find out in week one.

**Deliverables**
- A single Rust bin crate `spike/` that:
  1. Spawns `claude -p` (default) **and** `codex exec --json` (alt) via `tokio::process`, prompt on **stdin**, inheriting `HOME`/`PATH`.
  2. Sends a hardcoded schema summary + NL prompt ("show the 5 most recent orders").
  3. Parses stdout to a `{sql, rationale, is_write}` struct — tolerant last-message parser; regex-strips ```` ```sql ```` fences for Claude.
  4. Runs the returned SQL against a **local SQLite** file and a **local Postgres** (docker) over a **read-only session** (`SQLITE_OPEN_READONLY`; `BEGIN; SET TRANSACTION READ ONLY`).
  5. Feeds it a prompt that induces a write (`delete all orders`) and confirms the read-only session **rejects** it at the DB level.
- Notes file capturing: exact CLI invocation flags that worked, observed JSON shape per CLI, auth/preflight behavior, and any hang/timeout surprises.

**Rust modules involved:** `spike/main.rs` only (throwaway; concepts migrate to `agent`, `safety::l2`, `executor`).

**Exact success criteria (all must pass):**
1. ✅ `claude -p` **and** `codex exec --json` each return a parseable single SQL statement for an NL prompt, from a TTY-less spawned subprocess, using only the user's existing OAuth (no API key set).
2. ✅ The returned `SELECT` executes and returns rows against both local SQLite and local Postgres.
3. ✅ A write-inducing prompt's returned SQL is **rejected by the read-only DB session** (SQLite `SQLITE_READONLY`; Postgres `25006 read-only transaction`) — proving L2 is authoritative even when the model emits a write.
4. ✅ A hard `tokio::time::timeout` + process-group kill terminates a hung/slow child without orphaning it.
5. ✅ Startup preflight (`--version` + trivial auth check) distinguishes "authed & ready" from "needs login" **before** the query turn.
6. ✅ Measured: median wall-clock for one NL→SQL turn recorded for each CLI (sanity that latency is tolerable, not a target).

**Definition of done:** all six criteria demonstrated on one macOS dev machine, flags + JSON shapes written down, go/no-go decision recorded. `⟂ stretch:` also prove MCP stdio grounding (agent-db exposes one read-only `get_schema` tool) — but note the codex #15451 schema-drop interaction; defer if it costs more than a day.

---

## Phase 1 — Read-only MVP: connect, ask, see rows (safety foundation)

**Goal:** a native macOS app where a user connects to a DB, asks a question in natural language, and sees results — **read-only, fully audited**, with the approval gate stubbed (auto-run reads). This phase builds the skeleton and lands safety features **(1)** and **(3)**.

**Deliverables**
- Tauri v2 app shell, real repo layout (§ Repo layout below), Developer ID signing + notarization wired **now** (Keychain needs it).
- Connection manager: create/edit/test a Postgres or SQLite connection; secret stored in **Keychain** (`keyring-core` + `apple-native-keyring-store`), `secret_ref` in local `app.db`.
- Local app store (`app.db`) with schema from ARCHITECTURE §5.3; `connections`, `connection_safety`, `query_history`, `audit_log`, `schema_cache`.
- Schema introspection → `schema_cache`; relevance-filtered summary builder.
- Agent Bridge productionized from the spike: `AgentBackend` trait, `ClaudeBackend` (default) + `CodexBackend`, spawn hardening (process group, timeout, PATH probe, preflight).
- L1 parse/classify (`pg_query.rs` for PG, `sqlparser-rs` for MySQL/SQLite) — reject >1 statement, classify read/write/DDL, CTE-DML recursion.
- **L2 read-only enforcement** per engine (Postgres, SQLite for MVP; MySQL config present, exercised in Phase 4).
- **Audit log** — every NL prompt, generated SQL, classification, exec result written append-only + hash-chained.
- Results grid (TanStack Table + Virtual), streamed via `ipc::Channel<T>`; CodeMirror 6 read-only SQL viewer showing the generated statement.

**Rust modules:** `connection`, `agent` (`backend/`, `claude.rs`, `codex.rs`, `spawn.rs`), `safety` (`l1_parse.rs`, `l2_enforce.rs`), `audit`, `introspect`, `store`, `executor` (read path only), `commands`.

**Frontend screens:** Connection Manager, Connection form/test, NL Ask bar + generated-SQL panel, Results grid.

**Definition of done:**
- User connects to Postgres and SQLite, asks a question, gets correct rows — end to end in the packaged, notarized app.
- Every attempt (success, reject, error) appears in `audit_log` with an intact hash chain (a tamper test flips one row → chain verification fails).
- Any model-emitted write is blocked by L2 and surfaced as "blocked: read-only" — **never executed**.
- Query cancellation works (client + PG server-side cancel).
- `⟂ stretch:` MySQL connections; MCP live-grounding turn; ⌘K palette.

---

## Phase 2 — Approval gate + impact preview (arm safety, still no writes)

**Goal:** land safety features **(2)** and **(4)** as first-class UI and engine capabilities — but the write execution path stays **disabled**. This phase makes writes *previewable and approvable* without yet being *runnable*, so the gate is battle-tested before it guards anything real.

**Deliverables**
- **L4 Approval Card**: statement class + risk badge, dialect, full SQL, **plain-English restatement** (from the agent's `rationale`), parsed target tables/columns, `NO_WHERE` high-risk warning, timeout. Read-only SELECTs configurable to auto-run; anything classified write/DDL is **hard-gated** (but in this phase, "approve" on a write only runs the *preview*, not the write).
- **L3 impact preview engine**:
  - Reads → `EXPLAIN (FORMAT JSON)` / `EXPLAIN QUERY PLAN`, parsed to est. rows/cost.
  - Writes → open explicit txn, execute the real statement, capture `rows_affected()`, **unconditional `ROLLBACK`** → exact N. Flag `RETURNING`/function-referencing statements as "side-effects may fire pre-rollback".
- `connection_safety` wired to UI: `require_approval`, `explain_preview`, `max_rows`, auto-run-reads toggle.
- Approval decisions (who/when/approve|reject) recorded in `audit_log`.

**Rust modules:** `safety::l3_preview.rs`, `safety::l4_gate.rs`, `executor::tx.rs` (rollback harness), extend `commands`.

**Frontend screens:** Approval Card modal, risk/preview display, per-connection Safety Settings.

**Definition of done:**
- A write-classified statement produces an accurate **exact-N** preview via execute+rollback, and the DB row count is provably unchanged afterward (verified by a before/after count in a test).
- A read produces a plan-based estimate in the card.
- No path exists to COMMIT a write yet (asserted: `allow_writes` has no effect on execution in this phase; the commit branch is unimplemented/guarded).
- Approve/reject flows are audited.
- `⟂ stretch:` multi-turn "refine this query" using `--session-id`; NO_WHERE auto-suggest of a `WHERE`.

---

## Phase 3 — Enable writes (behind all four safety features)

**Goal:** flip on real write execution — now that read-only default (1), audit (3), approval gate (2), and tx-wrap+preview (4) are all present and exercised. Writes are opt-in per connection and always transactional.

**Deliverables**
- Write execution path: on explicit approval of a write, run inside `BEGIN … COMMIT` with the previewed statement; surface a final **Safe-Mode diff** (staged change) before COMMIT; rollback on any error or user cancel.
- Per-connection `allow_writes` gate (default **off**); writes impossible unless the connection has `allow_writes=1` **and** the user approves **and** preview succeeded.
- DDL handling: `DROP`/`ALTER`/`TRUNCATE` classified, extra-confirmation UI, least-priv-role guidance surfaced.
- Post-commit audit: actual rows affected vs. previewed N reconciled and logged.

**Rust modules:** `executor::write.rs` (commit path), extend `safety::l4_gate` (write confirm), `audit` reconciliation.

**Frontend screens:** Safe-Mode diff/commit view, DDL extra-confirm dialog.

**Definition of done:**
- An approved `UPDATE … WHERE` commits, and the audited "affected" matches the preview N.
- With `allow_writes=0` (default), no write can execute regardless of approval.
- Cancel mid-flight rolls back cleanly; a failed statement inside the txn leaves the DB unchanged (verified).
- Every write is present in `audit_log` with prompt, SQL, approver, preview-N, actual-N.
- `⟂ stretch:` batch/multi-statement scripts (still single-statement-per-approval under the hood).

---

## Phase 4 — Cloud providers + MySQL breadth

**Goal:** cover the connection-string cloud targets and MySQL/MariaDB — the "works with my real DB" phase.

**Deliverables**
- MySQL/MariaDB fully exercised (read + write + preview); `START TRANSACTION READ ONLY`, `max_execution_time` caveats handled.
- Provider profiles with their gotchas from ARCHITECTURE §5.2: **Supabase** (pooler 6543, `statement_cache_capacity(0)`, `postgres.<ref>` user), **Neon** (`channel_binding=require`, cold-start timeout, branch=host), **PlanetScale MySQL** (verify-identity, unreliable FK metadata, Vitess `EXPLAIN` → **preview marked lower-confidence in UI**), **PlanetScale Postgres**, **RDS** (custom CA bundle).
- Bundled CA roots (RDS global + ISRG); custom-CA field.
- Connection templates/wizard per provider.

**Rust modules:** `connection::providers.rs`, extend `safety::l2_enforce` (MySQL path), `introspect` (MySQL, PS FK-skip).

**Frontend screens:** provider-aware connection wizard, "lower-confidence preview" badge for PlanetScale.

**Definition of done:**
- Live connect + read + audited write-with-preview against Supabase, Neon, and PlanetScale (MySQL) test instances.
- PlanetScale write previews render with the low-confidence badge; FK introspection is skipped there without breaking schema view.
- `⟂ stretch:` **SSH tunnels (`russh`)** for bastion/VPC — flagged highest-risk custom component; may slip to Phase 5 if it threatens schedule.

---

## Phase 5 — Native polish → v1

**Goal:** make it *feel best on macOS* and harden for real daily use. This is the "genuinely native" bar from the brief.

**Deliverables**
- macOS-native chrome: ⌘K command palette, native menus/shortcuts, dark mode, window restoration, multi-connection tabs.
- Signed auto-update (`plugin-updater` + `latest.json`).
- Schema explorer sidebar (tables/columns/keys) with click-to-context for the agent.
- Snippets/saved queries (`snippets` table); query history browser with re-run.
- Error surfacing: verbatim `429`/rate-limit from the CLI, auth-expired → re-login prompt, cold-start hints.
- Perf: escalate results grid to Glide (canvas) only when >100k rows.
- Audit log viewer + export.
- Onboarding: CLI detection/preflight UI, least-priv role setup guidance.

**Rust modules:** `updater` glue, `commands` (history/snippets/audit-export), `agent::preflight` UI hooks.

**Frontend screens:** Command palette, Schema Explorer, Snippets, History, Audit Viewer, Onboarding/Preflight, Settings.

**Definition of done:**
- App passes a "native feel" review (menus, shortcuts, dark mode, window state), auto-updates from a signed feed, and a new user can go from launch → connected → first audited query via onboarding without docs.
- Rate-limit / auth-expiry / cold-start errors are legible and actionable.
- `⟂ stretch:` Glide canvas grid; MCP live-grounding as a per-connection opt-in (with codex #15451 guard); inline result-cell editing.

---

## Initial repo / module layout

```
agent-db/
├─ Cargo.toml                      # workspace
├─ ROADMAP.md  ARCHITECTURE.md  README.md
├─ spike/                          # Phase 0 throwaway (delete after Phase 1)
│  └─ src/main.rs
├─ src-tauri/
│  ├─ Cargo.toml
│  ├─ tauri.conf.json              # Developer ID, hardened runtime, NO sandbox, entitlements
│  ├─ entitlements.plist           # com.apple.security.cs.allow-jit
│  ├─ build.rs
│  └─ src/
│     ├─ main.rs                   # tauri::Builder, plugin + command registration
│     ├─ error.rs                  # AppError (typed, serialized to frontend)
│     ├─ state.rs                  # AppState: pools, agent config, app.db handle
│     ├─ commands/                 # #[tauri::command] boundary (thin, no logic)
│     │  ├─ mod.rs  connection.rs  query.rs  agent.rs  safety.rs  history.rs
│     ├─ agent/                    # THE BRIDGE
│     │  ├─ mod.rs                 # AgentBackend trait, AgentEvent, Final
│     │  ├─ spawn.rs               # tokio::process, process-group, timeout, PATH probe
│     │  ├─ claude.rs              # ClaudeBackend (default)
│     │  ├─ codex.rs               # CodexBackend
│     │  ├─ preflight.rs           # version + auth checks
│     │  └─ context.rs             # schema summary + PII redaction
│     ├─ safety/
│     │  ├─ mod.rs
│     │  ├─ l1_parse.rs            # pg_query.rs (PG) + sqlparser (MySQL/SQLite), classify
│     │  ├─ l2_enforce.rs          # per-engine read-only session/role   ← safety (1)
│     │  ├─ l3_preview.rs          # EXPLAIN / execute+ROLLBACK exact-N  ← safety (4)
│     │  └─ l4_gate.rs             # approval decision plumbing          ← safety (2)
│     ├─ executor/
│     │  ├─ mod.rs  read.rs        # paginate, stream, cancel
│     │  ├─ tx.rs                  # rollback harness (preview)
│     │  └─ write.rs               # commit path (Phase 3)
│     ├─ connection/
│     │  ├─ mod.rs  pool.rs        # PgPool/MySqlPool/SqlitePool per conn
│     │  ├─ providers.rs           # Supabase/Neon/PlanetScale/RDS profiles
│     │  ├─ keychain.rs            # keyring-core + apple-native-keyring-store
│     │  └─ ssh.rs                 # russh tunnel (Phase 4/5)
│     ├─ introspect/               # schema → schema_cache, relevance filter
│     │  └─ mod.rs pg.rs mysql.rs sqlite.rs
│     ├─ audit/                    # append-only hash-chained log   ← safety (3)
│     │  └─ mod.rs chain.rs
│     ├─ store/                    # app.db (AnyPool/SqlitePool), migrations
│     │  ├─ mod.rs
│     │  └─ migrations/
│     └─ mcp/                      # read-only introspection MCP server (stretch)
│        └─ mod.rs
├─ src/                            # frontend (React + TS + Vite)
│  ├─ main.tsx  App.tsx
│  ├─ ipc/                         # typed command wrappers + Channel handlers
│  ├─ screens/
│  │  ├─ Connections/  Ask/  Results/  Approval/  SafeModeDiff/
│  │  ├─ SchemaExplorer/  History/  AuditViewer/  Settings/  Onboarding/
│  ├─ components/                  # ApprovalCard, RiskBadge, SqlEditor(CM6), Grid
│  ├─ state/                       # TanStack Query
│  └─ lib/
├─ package.json  vite.config.ts  tsconfig.json
```

---

## Team-scale notes

- **Realistic ordering for 2–3 engineers:** Phase 0 (~1 week, one engineer) is a hard gate — do not staff UI work before it passes. Phases 1–2 are the bulk (safety engine + shell); resist starting Phase 3 writes until Phase 2's rollback preview is proven, since it's the cheapest place to find a data-loss bug.
- **Wire signing/notarization in Phase 1, not later** — Keychain (`-34018`) and the notarized-vs-dev-build JIT crash both bite only on signed builds; discovering them in Phase 5 is expensive.
- **Snapshot-test the CLI JSON contract** from Phase 1 on — the output shapes are unversioned and will drift (codex #15451, Claude fence-wrapping).
- **Stretch discipline:** SSH tunnels, MCP live-grounding, and the canvas grid are all `⟂ stretch`. Any of them can slip a phase without blocking v1; none sit on the safety critical path.