# ROADMAP-MCP.md — agent-db MCP Pivot

> Companion to **ARCHITECTURE-MCP.md** (2026-07-01). Phased build plan for a small team (2–3 engineers).
> **Sequencing rule, enforced throughout:** the read path, audit, and the in-app approval surface all ship **before** any write tool is enabled. The external agent can never write until a human can see and click.
> Reuses ARCHITECTURE §4 (safety L1–L4), §5 (connectivity), §7 (audit) wholesale behind the MCP tools. The old codex-shell-out chat path is removed (see § Removal plan).

---

## Sequencing principle

The MCP server exists to expose the **existing** safety pipeline to an external agent. So the order is not "build features" — it is "expose read, prove it, show it, audit it — *then* let the agent propose a write, and only through a human click."

| Capability | Reuses | Lands in |
|---|---|---|
| Localhost Streamable-HTTP MCP server + one read tool through the real pipeline | `rmcp`, `safety::l2`, `executor::read` | **Phase 0 (spike)** |
| Read-only tool catalog + live activity feed + `origin='mcp'` audit | `introspect`, `safety::l1/l2`, `audit`, existing grid/results | **Phase 1** |
| In-app approval surface + gated `run_write` (preview only reaches the human) | `safety::l3/l4`, `executor::execute`, `ApprovalCard.tsx` | **Phase 2** |
| Multi-platform onboarding/config generator + security hardening (token, origin, kill switch, bridge) | `store`, new `McpStatus` screen, bridge binary | **Phase 3** |
| Polish → v1 | — | **Phase 4** |

There is no phase in which the agent can execute a write without audit **and** the approval card both present and exercised.

---

## Phase 0 — Make-or-break spike: one real platform drives one real read tool

**Goal:** prove, with the smallest possible code on top of the *existing* app, that **one real subscription platform** connects to agent-db's local MCP server and that **a single read-only tool call executes through the real L1→L2 safety pipeline AND produces a visible UI reaction.** If this doesn't hold, the pivot thesis is wrong — find out in week one.

**Platform pick (per the transport decision):** **Cursor** (or **Claude Code** via `claude mcp add`). Both dial `127.0.0.1` Streamable HTTP directly today with zero brokering — the cleanest path. Claude Desktop is deliberately **out of scope for the spike** (it cannot dial localhost; it needs the stdio bridge, which is Phase 3). We de-risk the *happy path* first.

**Deliverables**
- A minimal `mcp` module wired into the running Tauri core (no removal work yet — the old Ask path can still sit there, dormant):
  - `serve_mcp(app, state, port)` spawns `rmcp` `StreamableHttpService` on `127.0.0.1:7686/mcp` on the existing tokio runtime (`tauri::async_runtime::spawn`).
  - `DbTools { app, state }` with **exactly one** read tool: `list_tables` (calls `introspect` on the active connection) — plus optionally `run_query` (SELECT) routed `l1_parse::classify` → `l2_enforce` (read-only session) → `executor::read`.
  - Each handler emits a Tauri event (`agent.tool_call`, `agent.result`) that the existing React frontend `listen()`s and renders (highlight the table / stream rows into Results). One temporary listener is fine.
- A one-page notes file: exact `rmcp` 1.8.x API surface that compiled (`StreamableHttpService::new`, `#[tool_router]`, `Parameters<T>`, `CallToolResult`/`McpError`), the working Cursor `~/.cursor/mcp.json` block, negotiated protocol version, and any handshake surprises.

**Exact steps**
1. Pin `rmcp = "1.8"` with `features = ["transport-streamable-http-server"]`; add `axum`, `tokio-util`, `schemars`. Compile a hello `#[tool]` to confirm the macro/type surface before writing real logic.
2. Implement `list_tables` → `introspect` on `state`'s active connection; return columns-once JSON + `structuredContent`.
3. In `lib.rs setup`, `state.app.set(handle)` then spawn `serve_mcp`. Bind `127.0.0.1:7686`; require an `Authorization: Bearer <token>` header (hardcoded token is acceptable for the spike).
4. Emit `agent.tool_call` on entry and `agent.result` on return; add a throwaway React listener that logs to the activity area and drives the existing grid/Results view.
5. Launch the app, connect to a **local Postgres** and a **local SQLite**. Point **Cursor** at `http://127.0.0.1:7686/mcp` with the bearer header. Open Cursor's chat and ask it to list tables / run a SELECT.
6. Watch the app window react live. Confirm the audit row is written with `origin='mcp'`.
7. **Adversarial read:** via `run_query`, have the agent attempt a write (`DELETE …`). Confirm L2 rejects it at the DB (PG `25006`), not just at classification.

**Success criteria (ALL must pass)**
1. Cursor (real, subscribed) completes the MCP handshake against the app-hosted `127.0.0.1:7686/mcp` and lists `agentdb`'s tools.
2. A `list_tables` (or `run_query` SELECT) tool call returns correct data to Cursor **through** `l1_parse` → `l2_enforce` → `executor::read` — verified by a log/breakpoint in each layer, not just a returned payload.
3. The **app window reacts visibly** to the call within ~1s: the touched table highlights and/or rows stream into the existing Results/grid view.
4. The call is written to `audit_log` with `origin='mcp'` and an intact hash chain.
5. A write attempted through `run_query` is **rejected by the read-only DB session** (PG `25006` / SQLite `SQLITE_READONLY`).
6. A missing/invalid bearer token yields `401` before any handler runs.

**stdio-bridge fallback (include in the spike if the clean HTTP path is uncertain).** If Cursor/Claude Code HTTP proves flaky, OR to pre-validate Phase 3's Claude Desktop path early: have the app also expose `DbTools` over a line-framed local TCP listener; build the ~30-line `agent-db-mcp-stdio` bridge (read port+token from `~/Library/Application Support/agent-db/mcp.json`, then `copy_bidirectional(stdin/stdout ↔ TcpStream)`, no MCP logic); point Claude Desktop's config at the bridge via **absolute path**; repeat criteria 1–4. Bridge success: Claude Desktop drives `list_tables` end-to-end while the GUI runs, dead-ends gracefully when it's down.

**Definition of done:** criteria 1–6 demonstrated on one macOS dev machine with a real Cursor (or Claude Code) subscription; `rmcp` API surface + working client config written down; go/no-go recorded.

---

## Phase 1 — MCP server skeleton + read-only tools + activity feed

**Goal:** a production MCP server exposing the **full read-only tool catalog**, every call through the real read pipeline, audited with `origin='mcp'`, rendered live in a new **activity-first UI**. No write tool exists yet. The old chat path is **removed** here.

**Deliverables**
- `src-tauri/src/mcp/`: `mod.rs` (`serve_mcp`, origin/token middleware, `CancellationToken` shutdown), `tools.rs` (`DbTools` + read `#[tool]`s), `events.rs` (event constants + emit helpers), `redact.rs` (PII masking relocated from `agent/context.rs`).
- Read-only catalog (all `readOnly`, all auto-gate): `list_connections` (id/name/engine/readonly/active only — never secrets/DSN/host), `list_tables`, `get_schema`, `get_object_details`, `get_table_rows`, `run_query`, `explain` (EXPLAIN, never ANALYZE).
- Token-aware serialization: columns-once `{cols, rows, truncated, total_estimate}` + `structuredContent`; caps 100/1000/~10k-token/~50KB; TEXT/BLOB truncated ~200 chars; NULL explicit; PII masking via `redact.rs`. **UI grid gets the full stream; the agent gets the capped payload.**
- `state.rs`: add `app: OnceCell<AppHandle>`, `pending: DashMap<Uuid, oneshot::Sender<Decision>>` (used Phase 2); **drop `agent`**.
- `store`: persist `mcp_port` (default 7686, `:0` on `EADDRINUSE`, read back + persist) and `mcp_token` (256-bit in `app.db`); write `~/Library/Application Support/agent-db/mcp.json`. Extend `audit_log`/`query_history` with `origin` + `client_id`.
- Frontend `src/screens/Activity/`: timestamped signal log; each entry click-jumps to its data view. Wire `agent.tool_call`/`agent.result` into the **existing** schema tree + `DataGrid.tsx` + Results (zero new view code). `App.tsx`: activity feed replaces the removed Ask rail.

**Modules / screens:** `mcp/` (new), `state.rs`, `store`, `commands` (+`mcp_status`); frontend `screens/Activity`, `App.tsx`, reuse `DataGrid`/Results/schema tree.

**Definition of done:**
- A real client calls every read tool and gets correct, capped, columns-once results; the feed logs each with client id + row count, and clicking jumps to its data view.
- Every read tool call writes an `audit_log` row with `origin='mcp'` + intact hash chain (tamper test fails verification).
- `run_query` with a write is blocked by L2 at the DB, surfaced as "blocked: read-only" — never executed.
- The old Ask panel and `ask_agent`/codex path are gone; the app still builds, runs, and works as a manual DB client.
- **No write tool is registered.**

---

## Phase 2 — Approval-gated writes + in-app approval surface

**Goal:** enable `run_write`, gated by L1→L3→L4 and a **human click inside the app**. Execution fires only from the in-app approval card — never from the MCP call itself.

**Deliverables**
- `src-tauri/src/mcp/approval.rs`: `Decision` enum, blocking round-trip with 300s timeout.
- `run_write` `#[tool]` (`destructive`), per ARCHITECTURE §5:
  - `l1_parse::classify` → if `l4_gate::decide == Block` (writes-off, etc.) return `McpError` **verbatim** (never reaches a human).
  - `l3_preview::preview` (execute-then-`ROLLBACK` exact-N; EXPLAIN-estimate above row threshold).
  - Insert `oneshot` into `state.pending`, emit `agent.awaiting_approval { build_proposal(...), proposal_id }`.
  - `timeout(300s, rx)`: Approve → `executor::execute(..., approved=true)` in `BEGIN…COMMIT`, emit `agent.result`, audit `applied`; Reject → `McpError("rejected by user")`; Timeout → remove pending (no leak), audit `timed_out`, return `{status:"pending", proposal_id}`.
- `get_change_status(proposal_id)` read tool → `pending|approved|rejected|applied|timed_out` (poll fallback for short-idle clients).
- `resolve_approval(id, approve)` Tauri command: `pending.remove(id)` → `tx.send(Decision)`.
- Frontend: reuse `ApprovalCard.tsx`, raised by `agent.awaiting_approval` — SQL + plain-English (`l4_gate::plain_english`) + risk badge + preview-N; Approve/Reject wired to `resolve_approval`. Per-connection `allow_writes` (default **off**) in the Safety tab.

**Modules / screens:** `mcp/approval.rs`, `mcp/tools.rs` (+`run_write`, `get_change_status`), `commands` (+`resolve_approval`); frontend `ApprovalCard.tsx` (re-wired), Safety tab.

**Definition of done:**
- With `allow_writes=1`, a `run_write UPDATE … WHERE` raises the card; Approve commits and audited actual-N reconciles vs preview-N; Reject returns `denied`, DB unchanged.
- With `allow_writes=0` (default), `run_write` returns a verbatim `McpError` and never raises a card or touches the DB.
- A >300s parked call returns `{status:"pending", proposal_id}`, pending entry removed (no leak), `get_change_status` reports the outcome.
- Every write path (blocked/rejected/timed-out/applied) is in `audit_log` with `origin='mcp'`, approver, preview-N, actual-N. Reads still never block.

---

## Phase 3 — Multi-platform onboarding/config generator + security hardening

**Goal:** make agent-db connectable from every target platform with copy-paste config, ship the stdio bridge for Claude Desktop, and harden the localhost port (token, origin, kill switch).

**Deliverables**
- **Bridge binary crate `agent-db-mcp-stdio`** (~30 lines): reads port+token from `mcp.json`, `copy_bidirectional(stdin/stdout ↔ TcpStream)`; app exposes the line-framed TCP listener in production. Bundled; configs reference it by **absolute path**.
- `src/screens/McpStatus/` (new): `Server: ON · 127.0.0.1:7686 · N clients`, **kill switch** (stops server + fires `CancellationToken`; also on window close), token reveal/copy/**rotate**, client count, optional idle auto-stop.
- **Config generator** with real port/token filled in: Cursor `~/.cursor/mcp.json`; VS Code `.vscode/mcp.json` (**`"type":"http"`** required); Windsurf `mcp_config.json` (**`serverUrl`**, not `url`); Claude Code (`claude mcp add --transport http …`); Codex `~/.codex/config.toml` (`url` + `bearer_token_env_var`, bridge fallback); **Claude Desktop** stdio-bridge block with absolute binary path.
- **Security hardening** (defense-in-depth around L2/L4): bind `127.0.0.1` only; bearer token required (`401` without), persisted in `app.db`, rotatable (rotate invalidates old configs). Strict **Host** + **Origin** validation (`403` unless Host ∈ {127.0.0.1:7686, localhost:7686} and Origin absent/allowlisted); **explicitly enable rmcp DNS-rebind protection** (ships off — GHSA-89vp-x53w-74fx). Allowlist ships empty; blocked origins surface a "allow this origin" one-click affordance in the feed. Kill switch + rotate + always-visible on/off + client count.
- Onboarding copy: "the GUI must be running" (bridge dead-ends otherwise); "schema/comments leave the machine via the external model provider" disclosure; prompt-injection ceiling = L2/L4.

**Modules / screens:** `agent-db-mcp-stdio` crate, `mcp/mod.rs` (origin/token/DNS-rebind mw, TCP listener), `store` (`mcp.json` writer, token rotate), `commands` (`rotate_token`, `set_server_running`); frontend `screens/McpStatus`, snippet generator, feed "allow origin".

**Definition of done:**
- Each of Cursor, VS Code, Windsurf, Claude Code, Codex connects using **only** the generated snippet; **Claude Desktop** connects via the bundled bridge and dead-ends gracefully when the GUI is down.
- Rotating the token `401`s the old config; the new snippet works.
- A browser page cannot reach the server (Host/Origin `403` + DNS-rebind on + token wall); a blocked legitimate client's Origin appears in the feed with a working one-click allow.
- Kill switch and window close both stop the server and drop clients; status always shows on/off + client count.

---

## Phase 4 — Polish → v1

**Goal:** make the visualizer feel native and legible for daily use.

**Deliverables**
- Activity feed: filter by client/tool, per-connection grouping, "active connection" clarity when multiple clients attach (revisit per-connection locking only if contention appears — §9.8).
- Approval UX: `NO_WHERE` emphasis, DDL extra-confirm, batching multiple pending proposals.
- Onboarding wizard: detect installed platforms, one-click config write, per-platform "server reachable?" preflight.
- Signed auto-update, audit viewer/export, macOS-native chrome (menus, ⌘K, dark mode, window restoration).
- Empirical hardening: capture real per-client Origin headers; measure client HTTP idle ceilings vs the 300s park; document `readOnlyHint` auto-run behavior per client.

**Definition of done:** launch → connect DB → generate config → external agent reads live → approve a write, without docs; auto-updates from a signed feed; per-client Origin/idle findings recorded; "native feel" review passes.

---

## Removal plan

Executed in **Phase 1** (read-only + audit already MCP-driven before anything is deleted).

**REMOVE**
- `src-tauri/src/agent/` — entire module (`codex.rs`, `spawn.rs`, `preflight.rs`, `mod.rs`). **Salvage** the PII-redaction logic from `agent/context.rs` → `mcp/redact.rs`.
- `commands::ask_agent` + its `generate_handler!` entry; the codex preflight block in `lib.rs setup`; `mod agent`; `AppState.agent: AgentConfig`.
- Frontend: `src/screens/Ask/`, the `askAgent` wrapper in `src/ipc/commands.ts`, Ask/agent-panel state (`showAgent`/`agentOpen`/`ai-panel`) in `App.tsx`.
- `Cargo.toml`: codex/subprocess deps (`command_group`, subprocess-timeout plumbing) **if unused elsewhere**.

**KEEP (reuse behind MCP tools)**
- `safety/` L1–L4; `executor/` (`execute(..., approved)` + `read`); `connection/` (pools, Keychain, tunnels); `introspect/`; `audit/`; `store/`; `model.rs`.
- Manual commands: `list_connections`, `upsert_connection`, `delete_connection`, `test_connection*`, `get_schema`, `refresh_schema`, `classify_sql`, `preview_sql`, `run_sql`, `get_safety`, `set_safety`, `list_audit`, `list_history`.
- Frontend `DataGrid.tsx`, `SqlViewer.tsx`, `ApprovalCard.tsx` (re-wired), Results/Tables/Audit/Safety/Connections screens.

---

## Updated repo / module layout

```
agent-db/
├─ Cargo.toml                      # workspace (+ agent-db-mcp-stdio member)
├─ ARCHITECTURE-MCP.md  ROADMAP-MCP.md  README.md
├─ agent-db-mcp-stdio/             # NEW ~30-line stdio→TCP bridge (Phase 3)
│  └─ src/main.rs                  # copy_bidirectional(stdin/stdout ↔ TcpStream)
├─ src-tauri/
│  ├─ Cargo.toml                   # + rmcp(1.8.x), axum, tokio-util, dashmap, schemars; − codex deps
│  └─ src/
│     ├─ lib.rs                    # setup: state.app.set + spawn serve_mcp; − codex preflight, − mod agent
│     ├─ state.rs                  # + app: OnceCell<AppHandle>, pending: DashMap<Uuid,oneshot>; − agent
│     ├─ error.rs  model.rs
│     ├─ commands/                 # + mcp_status, resolve_approval, rotate_token, set_server_running; − ask_agent
│     ├─ mcp/                      # NEW
│     │  ├─ mod.rs                 # serve_mcp: StreamableHttpService + axum /mcp + TCP listener; origin/token/DNS-rebind mw; CancellationToken
│     │  ├─ tools.rs               # DbTools{app,state} + #[tool] handlers (read catalog + run_write + get_change_status)
│     │  ├─ approval.rs            # Decision, 300s timeout round-trip
│     │  ├─ events.rs              # agent.tool_call / result / awaiting_approval / resolved + emit helpers
│     │  └─ redact.rs              # PII masking (relocated from agent/context.rs)
│     ├─ safety/                   # KEPT: l1_parse, l2_enforce, l3_preview, l4_gate
│     ├─ executor/                 # KEPT: read, write, tx  (execute() = single write entrypoint)
│     ├─ connection/  introspect/  audit/  store/   # KEPT
│     └─ (agent/  ← DELETED)
└─ src/
   ├─ App.tsx                      # activity-first layout; − Ask rail / agent-panel state
   ├─ screens/
   │  ├─ Activity/                 # NEW — signal log, click-jumps to data views
   │  ├─ McpStatus/                # NEW — server on/off, kill switch, token, per-platform config snippets
   │  ├─ Tables/ Results/ Sql/ Audit/ Safety/ Connections/   # KEPT (manual DB client)
   │  └─ (Ask/  ← DELETED)
   ├─ components/                  # KEPT: DataGrid, SqlViewer, ApprovalCard (re-wired to agent.awaiting_approval)
   └─ ipc/  (commands.ts − askAgent;  + agent.* listeners, resolve_approval)
```