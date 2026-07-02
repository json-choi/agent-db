# agent-db — MCP Pivot Architecture (ARCHITECTURE-MCP.md)

> Status: authoritative pivot design. Date: 2026-07-01. Author: lead architect.
> Supersedes ARCHITECTURE.md §1, §3 (agent bridge), and the "Ask panel" UI. §4 (safety L1–L4), §5 (data/connectivity), §6 (stack, minus the CLI/subprocess rows), and §7 remain in force and are **reused wholesale** behind the MCP tools.
> Decisions in this doc are made, not proposed. Where the four research reports disagreed, the resolution and its rationale are stated inline.

---

## 1. Product concept

agent-db stops being a chat client and becomes a **local MCP server with a live visualizer**. The conversation moves out to whatever platform the user already pays for — Claude Desktop, Claude Code, ChatGPT/Codex, Cursor, Windsurf, VS Code Copilot — and that external agent drives the database by calling agent-db's MCP tools (list schemas, browse rows, run read queries, explain, and propose gated writes). The **desktop app owns everything that must not leave the machine**: the connection pools, the Keychain-held credentials, the L1–L4 safety pipeline, and the hash-chained audit log. Its window becomes a real-time control surface — an activity feed of incoming tool calls, the schema/tables/rows the agent is touching, query results as they stream, and the approval card a human must click before any write executes. The agent proposes from outside; agent-db decides, executes, and shows, from inside. No LLM is ever billed by agent-db, and no write ever happens without an in-app human click.

---

## 2. Transport decision (the make-or-break)

**Decision: the GUI hosts one long-lived Streamable HTTP MCP server bound to `127.0.0.1:7686`, in-process in the Tauri Rust core. Clients dial it. A tiny stdio→HTTP bridge binary is shipped for stdio-only clients (Claude Desktop, and any client whose HTTP support proves flaky).**

Why not stdio as the primary: a stdio MCP server is *spawned by the client as a child process* and dies with it. That is fundamentally incompatible with "the GUI is already running and owns the pools, creds, and safety engine." All four reports converge here. Streamable HTTP (single `/mcp` endpoint, POST for calls, optional GET/SSE for streaming) is the one normative transport that lets a persistent, app-owned server accept multiple attaching clients. The deprecated dual-endpoint HTTP+SSE transport is **not** implemented.

**Protocol version:** negotiate **2025-11-25** (current stable) with backward compatibility down to 2025-03-26. We do **not** depend on any RC-2026-07-28-only feature (notably the Tasks extension) — R3's Tasks-based write-parking is treated as a future optimization, not a dependency. rmcp negotiates the version handshake for us.

**SDK / version pin:** official `rmcp` (`modelcontextprotocol/rust-sdk`), feature `transport-streamable-http-server`. **Pin to the last well-documented `1.8.x` stable line — do not float, do not adopt `2.0.0`** (published 2026-06-29, two days old, API drift unverified against every snippet we have). Re-evaluate 2.x only after it has settled and we can compile-check `StreamableHttpService::new`, `#[tool_router]`, `Parameters<T>`, `CallToolResult`/`McpError`. This resolves the R2 (2.0.0) vs R4 (1.8.x) split in favor of the boring, documented line.

### What works today vs. needs the bridge

| Platform | Direct localhost HTTP? | Config the app generates |
|---|---|---|
| **Cursor** | ✅ works today | `~/.cursor/mcp.json` → `mcpServers.agentdb.url` + `headers.Authorization` |
| **VS Code (Copilot)** | ✅ works today | `.vscode/mcp.json` → `servers.agentdb` with **`"type":"http"` (required)** or it execs the URL as stdio |
| **Windsurf (Cascade)** | ✅ works today | `~/.codeium/windsurf/mcp_config.json` → `serverUrl` (note: **not** `url`) |
| **Claude Code** (`claude mcp`) | ✅ works today | `claude mcp add --transport http agentdb http://127.0.0.1:7686/mcp` |
| **Codex CLI** (`codex mcp`) | ⚠️ works but flaky (openai/codex #11284, #15609) | `~/.codex/config.toml` `[mcp_servers.agentdb]` `url=` + `bearer_token_env_var=`; **keep bridge fallback** |
| **Claude Desktop** | ❌ **cannot** — remote connectors are cloud-brokered from Anthropic's infra, so `127.0.0.1` is unreachable and would force a public endpoint + OAuth 2.1 | **stdio bridge only** in `claude_desktop_config.json` |

**Claude Desktop is the decisive constraint.** It is the highest-value platform and it cannot dial localhost. Its only local path is stdio: the client spawns our bridge binary, the bridge reads the running app's port+token from `~/Library/Application Support/agent-db/mcp.json`, connects to `127.0.0.1:7686`, and pumps bytes. **The GUI must be running or the bridge dead-ends** — surfaced as an onboarding requirement. Bridge config must use an **absolute path** to the binary (GUI-spawned children get a minimal PATH; `npx`/short commands break).

**Bridge implementation (decisive, ~30 lines, zero MCP logic):** the app *also* exposes the same `DbTools` over a line-framed local TCP listener; stdio MCP framing equals that stream framing, so the bridge binary is a byte pump — `tokio::io::copy_bidirectional(stdin/stdout ↔ TcpStream)`. We do **not** reimplement MCP in the bridge, and we do **not** depend on Node/`mcp-remote`.

---

## 3. System architecture

```
 EXTERNAL PLATFORM (the chat lives here)
 Claude Desktop* / Claude Code / Cursor / Windsurf / VS Code / Codex
        │  JSON-RPC over MCP
        │  ├─ Streamable HTTP  →  http://127.0.0.1:7686/mcp   (Cursor, VSCode, Windsurf, Claude Code, Codex)
        │  └─ stdio → bridge binary → 127.0.0.1 TCP           (*Claude Desktop only)
        ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  agent-db  (Tauri v2 Rust core, one tokio runtime)                           │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────┐     │
│  │  mcp module  (NEW)                                                   │     │
│  │  rmcp StreamableHttpService on 127.0.0.1:7686  +  TCP for bridge     │     │
│  │  origin-check · bearer-token auth · #[tool] handlers (DbTools)       │     │
│  └───────┬───────────────────────────────────────────┬────────────────┘     │
│          │ every call re-enters the SAME pipeline     │ emits Tauri events   │
│          ▼                                             │                      │
│  ┌───────────────┐  ┌──────────────┐  ┌─────────────┐ │                      │
│  │ Safety L1–L4  │  │  Executor    │  │ Connection  │ │                      │
│  │ l1 classify   │─▶│ read / write │◀▶│ Manager     │ │                      │
│  │ l2 ro-session │  │ tx+rollback  │  │ pools+tunnel│ │                      │
│  │ l3 preview    │  │ execute()    │  │ Keychain    │ │                      │
│  │ l4 gate       │  └──────┬───────┘  └──────┬──────┘ │                      │
│  └───────┬───────┘         │                 │        │                      │
│          └─────────────────┴─────────────────┴────────┘                      │
│                     │                                                         │
│          ┌──────────▼──────────┐   Audit (append-only, hash-chained SQLite)   │
│          │  Introspection      │   + schema_cache + query_history             │
│          └──────────┬──────────┘                                             │
└─────────────────────┼───────────────────────────────────────┬───────────────┘
                      ▼                                         │ Tauri events +
              ┌───────────────┐                                 │ ipc::Channel<T>
              │  TARGET DBs   │                                 ▼
              │  Postgres     │                    ┌──────────────────────────┐
              │  MySQL/Maria  │                    │  REACT VISUALIZER (WKWV) │
              │  SQLite       │                    │  activity feed · schema  │
              │  Supabase/Neon│                    │  tree · row grid ·       │
              └───────────────┘                    │  results · APPROVAL card │
                                                   │  · MCP status/onboarding │
                                                   └──────────────────────────┘
```

Write path awaits a click: the write `#[tool]` handler emits `agent.awaiting_approval`, parks on a `oneshot` channel held in `AppState.pending`, and the approval card's `resolve_approval` command sends the decision back. Reads never block.

---

## 4. MCP tool catalog + safety mapping

Naming mirrors `crystaldba/postgres-mcp` so agent mental models transfer (adopting R3's set over R4's shorter list; DDL rides `run_write` rather than getting its own `apply_migration`). `connection` is **optional** — omitted means "the app's active connection" (the one selected in the UI). Every tool sets MCP annotations, emits a UI event, and writes one hash-chained `audit_log` row. **There is no `execute_sql` write tool** — writes cannot bypass L4 (ARCHITECTURE §3.4 hard rule).

| Tool | Args | Gate | Annotation | Reuses |
|---|---|---|---|---|
| `list_connections` | — | auto | readOnly | `store` — returns only `{id,name,engine,readonly,active}`, **never** secrets/DSN/host |
| `list_tables` | `connection?, schema?` | auto | readOnly | `introspect` + `schema_cache` |
| `get_schema` | `connection?, filter?` | auto | readOnly | `introspect` (relevance-filtered) |
| `get_object_details` | `connection?, object` | auto | readOnly | `introspect` |
| `get_table_rows` | `connection?, table, limit=100, offset?, filter?, sort?` | auto | readOnly | builds param'd SELECT → L1→L2→`executor::read` |
| `run_query` | `connection?, sql, max_rows?` | auto | readOnly | `l1_parse`→`l2_enforce`→`executor::read` |
| `explain` | `connection?, sql` | auto | readOnly | `l1_parse`→`l3_preview` (EXPLAIN, **never** ANALYZE) |
| `run_write` | `connection?, sql, note?` | **L4 in-app** | destructive | `l1`→`l3`→`l4_gate::decide`→`executor::execute` |
| `get_change_status` | `proposal_id` | auto | readOnly | reads `pending`/audit → `pending`\|`approved`\|`rejected`\|`applied`\|`timed_out` |

**Safety mapping (authoritative layers unchanged):**

- **Reads** → **L1** classify (reject non-read, reject statement-count > 1) → **L2** read-only session (authoritative: PG `SET TRANSACTION READ ONLY`, MySQL `START TRANSACTION READ ONLY`, SQLite `OPEN_READONLY`) → `executor::read`. `readOnlyHint:true` is **honest because L2 the DB enforces it**, not because the annotation says so. Annotations are advisory hints per MCP guidance; L2 and L4 remain the real boundary.
- **`run_write`** → **L1** classify write/DDL → **L3** execute-then-`ROLLBACK` for exact-N preview (EXPLAIN-estimate only above the row threshold, per DESIGN-REVIEW #3) → **L4** `decide()`: `allow_writes=false` → tool returns an error verbatim (blocked, never reaches a human); otherwise raise the in-app approval card. On approve → `executor::execute(..., approved=true)` in `BEGIN…COMMIT`, audit reconciles previewed-N vs actual-N.
- **Every** call → hash-chained `audit_log` with `origin='mcp'` + client id, plus a `query_history` row for replay.

**Serialization (token-aware, R3):** the agent receives compact **columns-once JSON** — `{cols:[…], rows:[[…]], truncated, total_estimate}` (array-of-arrays, ~40–60% fewer tokens than keyed objects) — plus `structuredContent`. Caps: default 100 rows, hard max 1000, and a ~10k-token / ~50KB byte budget, whichever hits first; large TEXT/BLOB truncated to ~200 chars; NULL explicit. **The UI grid receives all streamed rows regardless of the agent cap.** PII column masking (existing `agent/context.rs` redaction logic, retained as a `mcp` helper) applies to tool output.

---

## 5. Live-UI + blocking-approval event flow

Every handler emits Tauri events as it runs; React `listen()`s. Reuses the existing frontend event plumbing — the same events the old Ask path emitted, so **no new view code** for the reactive data views.

Event constants (`mcp/events.rs`): `agent.tool_call`, `agent.result`, `agent.awaiting_approval`, `agent.resolved`.

**Read (non-blocking):**
```
handler: emit agent.tool_call {tool, sql/table}
       → L1 → L2 read-only → executor::read
       → emit agent.result {rows, truncated}
       → return CallToolResult (agent gets compact rows; UI grid gets full stream)
```

**Write (blocking round-trip — decisive resolution of R2's block-with-timeout vs R3/R4's poll):**
```
run_write handler:
  cls = l1_parse::classify(sql)?
  if l4_gate::decide(settings, cls) == Block → return McpError (verbatim reason)  // writes-off etc.
  preview = l3_preview::preview(...)?                        // EXPLAIN + rollback exact-N
  id = Uuid::new_v4();  (tx, rx) = oneshot::channel();  state.pending.insert(id, tx);
  emit agent.awaiting_approval { l4_gate::build_proposal(sql, cls, Some(preview)), proposal_id: id }
  match timeout(300s, rx):
    Approve → executor::execute(live, engine, cls, sql, settings, approved=true)
              emit agent.result; audit "applied"; return CallToolResult
    Reject  → return McpError("rejected by user")
    Timeout → state.pending.remove(id); audit "timed_out";
              return CallToolResult{ status:"pending", proposal_id }   // client polls get_change_status
```

**Resolved by a Tauri command the approval card calls:**
```rust
#[tauri::command]
fn resolve_approval(state: State<Arc<AppState>>, id: Uuid, approve: bool) {
    if let Some((_, tx)) = state.pending.remove(&id) {
        let _ = tx.send(if approve { Decision::Approve } else { Decision::Reject });
    }
}
```

**Decision — block-with-timeout is primary; polling is the fallback.** The MCP call parks up to 300s so the agent gets a clean synchronous answer (best UX for clients that hold the call open). If it times out, the call returns `{status:"pending", proposal_id}` and the agent polls `get_change_status` — which also covers clients with short HTTP idle limits. This unifies R2 (block) and R3/R4 (poll) instead of choosing one. The timeout arm removes the `pending` entry so the awaited task never leaks.

---

## 6. Security model

The DB session (L2) and the human gate (L4) remain the authoritative boundaries; the network layer is defense-in-depth around a naked localhost port that any local process or browser page can reach.

| Threat | Defense |
|---|---|
| Local process scans the port | Bind **`127.0.0.1` only** (never `0.0.0.0`). **Bearer token** in `Authorization` — 401 without it. Token = 256-bit random, **persisted in `app.db`** (survives restart so pasted configs keep working), rotatable + revocable from the UI. *(Resolves R1 "per-install" vs R4 "per-start": persist, but expose rotate.)* |
| Browser page → DNS-rebinding to localhost | **Strict Origin + Host validation (spec MUST):** 403 unless `Host` ∈ {`127.0.0.1:7686`,`localhost:7686`} and `Origin` is absent or allowlisted. **Explicitly enable rmcp's DNS-rebind protection** — SDKs ship it *off* (GHSA-89vp-x53w-74fx / CVE-2025-66416). Browsers can't set `Authorization` cross-origin without a preflight we decline → token is the second wall. |
| Confused/malicious agent issues writes | Writes are **never** MCP-executable. `run_write` only previews + raises the card; execution fires solely on in-app Approve. L2 read-only session rejects any write leaking through `run_query` at the DB (PG error `25006`). |
| Data exfiltration | Read-only-by-default per connection (`allow_writes=0`); `max_rows` cap; PII masking on tool output; **every call audited** and shown live in the activity feed for human oversight. |
| Prompt injection (DB content → agent) | Untrusted by design — injected instructions can only call the same gated tools. Damage ceiling = L2/L4, not the prompt. Flagged in onboarding. |
| Stale/leaked token, forgotten server | **Kill switch** in the status panel stops the server + fires the `CancellationToken` (also on window close). Rotate token invalidates old configs. Status bar always shows on/off + connected-client count. Optional idle auto-stop. |

**Origin allowlist ships empty by default:** native clients send no `Origin` (pass); browser origins are blocked. When a real client is caught by Host/Origin checks, the activity feed shows a "blocked origin" event with a one-click "allow this origin" — because exact per-platform Origin values over Streamable HTTP are undocumented (flagged as needs-testing per client).

---

## 7. App UI

**REMOVED:** the Ask panel (`src/screens/Ask/`), the `showAgent`/`agentOpen`/`ai-panel` chrome in `App.tsx`, and the chat-thread UX. The conversation is gone from the app entirely.

**Replaced by an activity-first layout:**

- **(a) Activity feed (new primary rail, where chat was):** timestamped tool-call log — `14:02 claude-desktop · run_query users → 12 rows`, `14:03 · awaiting approval: UPDATE orders SET…`. Not a conversation — a **signal log**. Each entry click-jumps to its data view.
- **(b) Reactive data views (KEPT, now MCP-driven):** the existing schema tree + TanStack row grid + Results view, driven by `agent.tool_call`/`agent.result` over the existing `ipc::Channel<T>`. When the agent reads `users`, the app highlights that table and streams results into Results live. Zero new view code.
- **(c) Approval surface (KEPT `ApprovalCard.tsx`, now agent-triggered):** SQL + plain-English restatement + risk badge + preview-N, raised by `run_write`, with Approve/Reject wired to `resolve_approval`. Reject → tool returns `denied`.
- **(d) MCP status / onboarding panel (new):** `Server: ON · 127.0.0.1:7686 · 1 client`, kill switch, token (reveal/copy/rotate), and **per-platform copy-paste snippets** with the real port/token filled in (including the Claude Desktop stdio-bridge block with the absolute binary path).
- **(e) KEPT for direct human use:** the Data / SQL / Results / Audit / Safety tabs and the connection manager — unchanged. agent-db is still a usable manual DB client with the agent switched off.

---

## 8. Code: remove / keep / add

**REMOVE**
- `src-tauri/src/agent/` — entire module (codex spawn, context, preflight, spawn). *(Keep the PII-redaction logic from `agent/context.rs` — relocate it to `mcp/redact.rs`; it's reused for tool-output masking.)*
- `commands::ask_agent` + its `generate_handler!` entry; the codex preflight block in `lib.rs setup`; `mod agent`; `AppState.agent: AgentConfig`.
- Frontend: `src/screens/Ask/`, the `askAgent` wrapper in `ipc/commands.ts`, and Ask/agent-panel state in `App.tsx`.
- Cargo: codex/subprocess deps (`command_group`, subprocess timeout plumbing) if unused elsewhere.

**KEEP (reuse behind MCP tools, unchanged)**
- `safety/` L1–L4 — `l1_parse::classify`, `l2_enforce`, `l3_preview::preview`, `l4_gate::decide`/`build_proposal`/`plain_english`.
- `executor/` — `executor::execute(live, engine, classification, sql, settings, approved)` is the single write entrypoint the write handler calls after approval; `executor::read` for reads.
- `connection/` (pools, Keychain, tunnels), `introspect/`, `audit/` (hash chain), `store/`, `model.rs`.
- Existing commands: `list_connections`, `upsert_connection`, `delete_connection`, `test_connection*`, `get_schema`, `refresh_schema`, `classify_sql`, `preview_sql`, `run_sql`, `get_safety`, `set_safety`, `list_audit`, `list_history` — still power the manual tabs.

**ADD**
- `src-tauri/src/mcp/` — `mod.rs` (`serve_mcp(app, state, port)`: `StreamableHttpService` + axum `/mcp` + the line-framed TCP listener for the bridge; origin/token middleware; graceful shutdown via `CancellationToken`), `tools.rs` (`DbTools{app,state}` + `#[tool]` handlers calling the kept modules), `approval.rs` (`Decision`, timeout), `events.rs` (event constants + emit helpers), `redact.rs` (moved from agent).
- `state.rs`: `app: OnceCell<AppHandle>` (set in `setup`), `pending: DashMap<Uuid, oneshot::Sender<Decision>>`; drop `agent`.
- `lib.rs setup`: `state.app.set(handle)`, then `tauri::async_runtime::spawn(serve_mcp(...))` on the existing tokio runtime (no second runtime). Add `resolve_approval`, `mcp_status`, `rotate_token`, `set_server_running` to `generate_handler!`.
- `store`: persist `mcp_port` (default 7686, ephemeral `:0` fallback on `EADDRINUSE`, read back + persist) and `mcp_token`; write `~/Library/Application Support/agent-db/mcp.json` for the bridge/UI snippets. Extend `audit_log`/`query_history` with `origin='mcp'` + client id.
- New bridge binary crate `agent-db-mcp-stdio` (~30 lines, `copy_bidirectional`).
- Frontend: `src/screens/Activity/` (feed), `src/screens/McpStatus/` (status + onboarding snippets), event listeners wiring `agent.*` into the existing grid/results/approval components.
- `Cargo.toml`: `rmcp` (pinned 1.8.x, `transport-streamable-http-server`), `axum`, `tokio-util`, `dashmap`, `schemars`.

---

## 9. Ranked risks & open questions

1. **Claude Desktop cannot dial localhost (highest).** The top-value platform needs the stdio bridge + port/token discovery + "GUI must be running" dance; if the app is down, the bridge dead-ends. Mitigation: bundle our own bridge (no Node/PATH dependency), absolute paths in generated config, clear onboarding that the app must be open. *Open: monitor whether Anthropic ever ships a local-HTTP path.*
2. **rmcp version churn.** 2.0.0 is two days old and undocumented against our snippets; 1.8.x is the safe pin but we must compile-verify `StreamableHttpService::new`, `#[tool_router]`, `Parameters<T>`, `CallToolResult`/`McpError` before committing. *Open: exact 1.8.x API surface + built-in DNS-rebind toggle name.*
3. **Codex Streamable-HTTP maturity.** openai/codex #11284/#15609 show local-HTTP support is real but flaky mid-2026. Mitigation: validate empirically; keep the stdio bridge as Codex's fallback too.
4. **Per-client Origin/Host values are undocumented.** Strict validation could 403 a legitimate client. Mitigation: empty allowlist (header-absent native clients pass), plus a "blocked origin → allow" affordance in the feed. *Open: capture real Origin headers per client during testing.*
5. **Blocking write call vs. client HTTP idle timeouts.** A 300s parked call may exceed some clients' idle limits. Mitigation: the timeout arm returns `{pending, proposal_id}` and `get_change_status` polling covers it. *Open: measure real idle ceilings per client; consider the RC Tasks extension later.*
6. **DB schema/comments still leave the machine** via the external platform's model provider — same disclosure as ARCHITECTURE §3.5, now more prominent because the whole conversation is off-box. Mitigation: document plainly in onboarding; PII masking on row output.
7. **Auto-approve honesty.** Whether Claude Desktop/ChatGPT honor `readOnlyHint` to auto-run reads vs. prompt each time is unknown — a UX, not a safety, question (L2 makes reads safe regardless). *Open: test per client.*
8. **Multi-client concurrency.** Multiple platforms can attach to one server; the current single global `connections` Mutex and one active connection may serialize or confuse "active connection" semantics. Mitigation: `connection` arg is explicit-capable; revisit per-connection locking only if contention appears (existing ponytail note in `state.rs`).

**Resolved where reports disagreed:** transport → app-hosted Streamable HTTP + stdio bridge (unanimous); protocol → 2025-11-25, no RC-only deps (R4 over R3's RC); SDK → rmcp 1.8.x pinned (R4 over R2's 2.0.0); write round-trip → block-with-timeout primary + poll fallback (unifies R2 and R3/R4); token → persisted + rotatable (R1 persistence + R4 revocability); port → 7686 (R2 over R4's 7654); tool set → R3's 9-tool crystaldba-aligned catalog.