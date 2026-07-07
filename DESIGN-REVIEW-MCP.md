# Principal-engineer review — dopedb MCP pivot (pre-Phase-1)

Reviewed against the actual tree (`executor::execute`, `l4_gate::decide`, `l3_preview::preview`, `state.rs`, `SPIKE-0-RESULTS.md`) and against 2026 platform/SDK reality (web-verified, not memory). The design is unusually well-grounded — signatures cited in the doc match the code, the safety layering is real, and the sequencing rule (read+audit+approval-surface ship before any write tool) is correct. My job is to break it. Findings ranked most-severe first.

## 1. The riskiest assumption partially FAILS: two of the highest-value surfaces (Claude Desktop, ChatGPT) do not work on the clean transport — and ChatGPT is quietly dropped

The doc's transport bet is: app-hosted Streamable HTTP on `127.0.0.1:7686`, clients dial in. Verified 2026 status:

- **Cursor, VS Code Copilot, Windsurf, Claude Code** — dial localhost Streamable HTTP directly. **Solid, confirmed.** Affirm this path.
- **Claude Desktop** — the official MCP "connect local servers" flow is **stdio only** (`mcpServers.command` spawns a child; Streamable HTTP is documented for *remote/cloud* servers). The doc's claim "Claude Desktop cannot dial localhost, needs the stdio bridge" is **correct.** But it is then **deferred to Phase 3**, i.e. the single highest-value platform's only working path is the *last* thing built and is validated last.
- **ChatGPT** — the pivot brief names "ChatGPT/Codex" as a target. Verified: the **ChatGPT app supports only remote MCP servers over public HTTPS with OAuth; it does not support local stdio and cannot reach `127.0.0.1`** (you must expose via tunnel/ngrok/Cloudflare + OAuth). The architecture silently narrows "ChatGPT" down to "Codex CLI (flaky)" in its table and never states that the **ChatGPT desktop/web product is out of scope** without a public endpoint — which the whole design refuses to build. That is a requirements gap, not a detail.

Net: of the two flagship *consumer* subscription surfaces the pivot is pitched at, one (Claude Desktop) works only through the deferred bridge, and the other (ChatGPT app) does not work at all under the stated "no public endpoint" constraint.

**Demand before Phase 1:** expand Phase 0's exit criteria to *also* prove the stdio bridge end-to-end against real Claude Desktop (the doc already sketches this as optional — make it mandatory). Minimal test: build the ~30-line `copy_bidirectional` bridge, point `claude_desktop_config.json` at it by absolute path, drive `list_tables`, kill the GUI and confirm graceful dead-end. And make an explicit, written scope decision on ChatGPT: "Codex CLI only; ChatGPT app requires a tunnel we don't ship" — don't let the table imply ChatGPT-app works.

## 2. L3 preview EXECUTES the agent's write before any human clicks — real effects escape L4 on non-transactional tables and triggers

`run_write` runs `l3_preview::preview` *before* raising the approval card. For `QueryKind::Write`, preview calls `exec_rollback` (`tx.rs`) which **actually runs the statement** and relies on `ROLLBACK` to undo it. This is safe only when the target is transactional. It is **not** safe for:

- **MySQL MyISAM/MEMORY tables** — non-transactional; the UPDATE/DELETE commits and `ROLLBACK` is a no-op. The write lands **before the human sees the card.**
- **Triggers with external side effects / `RETURNING`** — the code itself flags NOTIFY/dblink/trigger-body writes fire before rollback.

In the old product a human *typed* that SQL. In the pivot, an **external, prompt-injectable agent** supplies it and preview fires automatically. So an injected `UPDATE myisam_audit SET ...` produces real, un-approved, un-rolled-back mutation the instant the tool is called — bypassing the "no write without a click" guarantee. DDL/privilege correctly skip exec-preview (verified: `PreviewMode::Skipped`), so this is narrower than "all writes," but it is a genuine hole that widens under external drive.

**Fix:** detect engine/table transactionality (or gate exec-preview to Postgres/SQLite + InnoDB-only) and fall back to EXPLAIN-estimate otherwise; keep exec-preview off for any statement `side_effect_note` flags. Prove it in Phase 2 with a MyISAM table in the test matrix.

## 3. The bearer token is not an authorization boundary against local processes — and reads have *no* gate

The threat table sells the token as the defense against "a local process scans the port." It isn't. The token is written to `~/Library/Application Support/dopedb/mcp.json` and `app.db`, both readable by **any process running as the same user** (macOS has no per-process ACL there). So any co-resident malware/tool reads the token, sends valid requests, and — because **reads auto-run with no approval** — pulls **every row of every configured connection** via `run_query`/`get_table_rows`. The `connection?` arg lets it target connections not even visible in the UI. L4 protects writes; **nothing protects reads from a local caller.** That's inherent to a localhost DB server, but the doc must say so plainly instead of implying the token stops local processes. The token's real job is narrower: it blocks *remote* callers and *browser* pages (which can't set `Authorization` cross-origin without a preflight you decline) — that part is sound.

**Fix the model, not necessarily the code:** relabel the token as "anti-browser / anti-remote, not anti-local-process"; state that any local user-process is inside the trust boundary for reads; consider tightening file perms on `mcp.json` (0600) as hygiene (doesn't stop same-user, but stops other-user). This is a documentation-honesty + `chmod` item, not a rearchitecture.

## 4. Timeout-then-approve silently drops the write (correctness bug in the block+poll unification)

The write flow: park on `oneshot` for 300s; on timeout, `state.pending.remove(id)` and return `{status:"pending"}`. **But the approval card is still on screen.** If the human clicks Approve at 301s, `resolve_approval` does `pending.remove(id)` → `None` → no-op. The write **never executes**, yet `get_change_status` reports `timed_out` and the agent has moved on. A human who approves a "timed-out" write gets silent nothing. This is exactly the seam where R2 (block) and R3/R4 (poll) were stitched, and the stitch leaks.

**Fix:** the Approve path must be able to execute independently of the parked `oneshot`. Persist the proposal (sql+classification+preview) keyed by `proposal_id`; on click, execute from the persisted proposal whether or not the original call still waits; `get_change_status` then reports `applied`. Decide and specify this **before Phase 2**, it's not a polish item.

## 5. "App genuinely reacts live" is only half-true under `connection?` + multi-client

`connection` is optional and can name *any* configured connection by id. Multiple clients can attach. The visualizer shows the UI-selected active connection. So an agent can read/propose-write against connection **B** while the window is showing **A** — the live-reaction and the approval card can render for a DB the operator isn't looking at, defeating "human sees what's happening before approving." The doc's risk #8 waves at this; for the approval path it's a safety issue, not just UX.

**Fix (cheap):** every activity entry and — critically — the approval card must name the connection explicitly and, on a write, either force-focus that connection in the UI or refuse to render the card as approvable until the operator is looking at the right connection. Small diff, closes the confusion.

## 6. rmcp DNS-rebind claim is factually wrong (in your favor) — verify the pin, don't call `disable_allowed_hosts`

The doc says "SDKs ship DNS-rebind protection *off* — explicitly enable it." Verified: since **rmcp 1.4.0, Host validation defaults ON** with loopback allowlist `["localhost","127.0.0.1","::1"]` (GHSA-89vp-x53w-74fx fix). Only **Origin** validation is opt-in (`allowed_origins`). So on your 1.8.x pin, Host protection is **already on by default** — the actual risk is someone accidentally calling `disable_allowed_hosts()` or setting `mcp_port` and forgetting `localhost:PORT` isn't the check (the allowlist is host-only, port-agnostic). Correct the doc, add Origin allowlist as designed, and add a startup assertion/test that `allowed_hosts` is non-empty. Minor, but a security doc with a wrong premise invites wrong code.

## 7. macOS specifics — mostly handled, three real gaps

- **Sandbox/entitlements:** a Tauri app that *listens* needs `com.apple.security.network.server` (plus `network.client` for DB egress) if sandboxed for the Mac App Store. If you ship outside the MAS with hardened runtime, listening is fine but the **bundled bridge binary must be signed + notarized** — Claude Desktop spawns it, and Gatekeeper will kill an unsigned/quarantined child. Put this in Phase 3's DoD.
- **Port fallback vs. pasted config:** `:0` on `EADDRINUSE` is correct, but every generated snippet hardcodes `7686`. After a fallback the config generator must emit the *actually-bound* port (doc says it reads it back — make the snippet regeneration a tested step, and warn that a previously-pasted config is now stale).
- **Bridge PATH:** absolute-path requirement for the bridge binary is right and confirmed necessary (GUI-spawned children get a minimal PATH).

## Tech choices — affirmed

- **Streamable HTTP + stdio bridge:** correct, unanimous, matches 2026 reality. Affirm.
- **rmcp 1.8.x pin over 2.0.0 (2 days old):** correct laziness. Affirm; re-evaluate 2.x after it settles.
- **Protocol 2025-11-25, no RC-only (Tasks) deps:** correct — don't build on `RC-2026-07-28`.
- **Bridge as byte-pump over line-framed TCP (no Node/`mcp-remote`):** good — avoids the `mcp-remote` dependency and PATH breakage. Affirm.
- **Reusing L1–L4/executor/audit wholesale:** correct and the whole point; code confirms the entrypoints exist as claimed.

## Must-answer before Phase 1

1. **Claude Desktop bridge: proven or not?** Move it into Phase 0 exit criteria. If it doesn't work end-to-end, the pivot's top surface is unvalidated.
2. **ChatGPT: in or out?** Written scope decision. If "in," you're building a public-endpoint+OAuth path the design explicitly rejects — that changes the whole security model.
3. **Timeout-then-approve:** does an Approve after 300s execute? Specify the persisted-proposal path (finding #4) before writing `run_write`.
4. **Exec-preview safety on non-transactional targets:** what's the engine/table gate (finding #2)? Add MyISAM + a trigger to the test matrix.
5. **Client HTTP idle ceilings:** measure real values for each target against the 300s park (Phase 0 can capture this for Cursor/Claude Code for free).
6. **Per-client Origin/Host headers:** capture real values during the spike so strict validation doesn't 403 a legit client on day one.
7. **Token file perms + honest threat wording:** `chmod 600 mcp.json`, and rewrite the "local process" row (finding #3).
8. **Reads exfil acceptance:** explicitly accept that any local process = full read of all connections, and that schema/comments leave the box via the external model provider. Put both in onboarding.

## Verdict: **CONDITIONAL GO — to Phase 0 only.**

The thesis is sound and the reuse story is real; `SPIKE-0-RESULTS.md` already proves subscription CLIs return clean SQL with no API key and that L2 read-only holds. **Go build the Phase 0 spike.** But **No-Go on staffing Phase 1** until Phase 0's exit criteria are expanded to include (a) the Claude Desktop stdio bridge proven end-to-end — not deferred, it's the highest-value surface; (b) an explicit ChatGPT in/out decision; (c) a specified fix for the timeout-then-approve drop; and (d) the exec-preview transactionality gate. Findings #2 and #4 are correctness/safety defects that must be designed out before any write tool ships; #1 is the make-or-break that must be de-risked in week one, not Phase 3.

Sources: [modelcontextprotocol.io — connect local servers](https://modelcontextprotocol.io/docs/develop/connect-local-servers), [OpenAI — Developer mode & MCP connectors in ChatGPT](https://help.openai.com/en/articles/12584461-developer-mode-apps-and-full-mcp-connectors-in-chatgpt-beta), [rmcp DNS-rebinding advisory GHSA-89vp-x53w-74fx](https://github.com/modelcontextprotocol/rust-sdk/security/advisories/GHSA-89vp-x53w-74fx), [rmcp Origin-validation issue #822](https://github.com/modelcontextprotocol/rust-sdk/issues/822).