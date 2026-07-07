# dopedb — Pre-Build Design Review (skeptical pass)

I pulled current (2026) billing/ToS facts before writing; they materially change the top finding. Sources at the end.

---

## 1. [CRITICAL] The "use your subscription, no API keys" thesis is already partly false as of today — and the default CLI is the wrong one for it

**Risk.** The product's entire premise is "don't bill our own LLM API; shell out to the CLI the user already pays for." Current reality on 2026-07-01:

- **Anthropic (the chosen default):** As of **June 15, 2026**, `claude -p` is explicitly reclassified as *programmatic usage*. It **no longer draws from the interactive subscription limits** — it draws from a **separate Agent SDK credit pool** ($20/mo on Pro, $100 Max 5x), metered at **API list prices**, and when that pool is exhausted, requests **stop** (unless the user has separately enabled pay-per-token usage credits). Separately, since **Feb 19, 2026** Anthropic states the Agent SDK requires API-key auth and OAuth subscription tokens are "not permitted." So the default backend is the one vendor that has actively walled `claude -p` off from the subscription you're claiming to reuse.
- **OpenAI (the "alternative"):** `codex exec` CLI usage **still draws from the ChatGPT subscription's rolling 5-hour message window** — i.e. it *is* the thesis-compatible backend, and you demoted it to alternate.

**Why it matters.** The headline "No separate API keys" is not true for the default backend today. A user on Pro will burn their $20 Agent SDK credit in a modest number of NL→SQL turns (each turn ships a schema-context prompt = non-trivial input tokens) and then the app simply stops working. That's not "ToS friction" (as the arch's risk #1 frames it) — it's a hard product-breaking economic ceiling that already shipped.

**Recommendation.**
- Demote "No separate API keys" from a headline differentiator to "works with your subscription *where the vendor allows it, subject to their programmatic credit pool*." Make the **API-key path first-class, not `optional`** — it's the only path with predictable economics on Anthropic.
- **Flip the default to codex**, or present both neutrally and pick the default at runtime based on which the user is actually authed into. Defaulting to the backend with the worst subscription-backed economics is backwards.
- Surface remaining Agent-SDK-credit / 5-hour-window state in the UI as a first-class number, not a `429` string after the fact.

## 2. [CRITICAL] Phase 0 spike does NOT de-risk the actual riskiest assumption

**Risk.** Phase 0's six criteria all test **plumbing** — spawn TTY-less, parse JSON, read-only session rejects a write, timeout kills the tree. None of that is where the product dies. The spike proves a *single* query returns SQL once; it never touches **quota burn, cost per turn, behavior at exhaustion, or ToS permissibility** — which finding #1 shows is the thing most likely to kill the thesis. It even measures latency ("sanity") but not the one number that matters: how many real questions a $20 Pro credit / one 5-hour window buys.

**Recommendation — add a 7th criterion and run it on a *real* paid subscription, not a test key:**

```bash
# Anthropic — confirm subscription path runs post-June-15 with NO api key, and measure cost/pool burn
unset ANTHROPIC_API_KEY
for i in $(seq 1 20); do
  printf '%s\n' "$SCHEMA_CTX show the 5 most recent orders" \
  | claude -p --output-format json --model sonnet \
  | jq -c '{turn:'"$i"', cost:.total_cost_usd, usage:.usage}'
done
# Then eyeball console.anthropic.com Agent-SDK credit balance before/after,
# and keep going until it refuses — capture the EXACT refusal payload.

# OpenAI — measure 5-hour message-window burn to exhaustion
codex login status
for i in $(seq 1 40); do
  printf '%s\n' "$SCHEMA_CTX show the 5 most recent orders" \
  | codex exec --json -s read-only --skip-git-repo-check - \
  | jq -c 'select(.type=="turn.completed") | {turn:'"$i"', usage:.usage}'
done
# Capture the 429 / limit payload verbatim when the window drains.
```

**Success = you can state, per CLI:** (a) does the pure-subscription (no-API-key) path still function, (b) queries-per-refresh-cycle for a realistic schema-context prompt, (c) the exact machine-readable exhaustion signal to render in the UI. If (b) is single-digit on Anthropic Pro, that's a go/no-go input for the *whole product*, and it belongs in week one — which is exactly what Phase 0 claims to be for.

## 3. [HIGH] The gate's biggest bypass isn't SQL classification — it's the agent subprocess's own tools

**Risk.** The arch correctly forbids an `execute_sql` MCP tool. But the spawned `claude`/`codex` process is a **full agent with a shell, filesystem, and network**, launched with **inherited `HOME`/`PATH`/env**. The safety model assumes the agent "only proposes SQL," yet nothing structurally stops the model from using its Bash/file tools to read `~/.pgpass`, the app's `app.db`, env-var DSNs, or `~/.config` connection strings, open its *own* connection to the target DB, and run whatever it wants — entirely outside L1–L4. The audit log wouldn't even see it. "The agent never touches the database directly" is an assumption, not an enforced boundary.

**Why it matters.** This is the literal answer to "where could the AI bypass the gate?" — it doesn't need a writable-CTE trick when it has `bash` and the same credentials the user gave the app.

**Recommendation.**
- Run the generation turn with **tools stripped**: codex `-s read-only` is not enough (it still allows reads/exec in a sandbox) — use the tightest sandbox/`--allowedTools ""` equivalent so the child can emit text only. For Claude, `--permission-mode plan` blocks file *edits* but not Bash/network reads; pass an explicit empty `--allowedTools` (or `--disallowedTools`) so no tool runs.
- **Scrub the environment**, don't inherit it: pass only what OAuth needs (`HOME` for `~/.claude`/`~/.codex`, minimal `PATH`), and strip `PG*`, `DATABASE_URL`, `MYSQL_*`, `*_TOKEN`, etc. There's a real tension here (OAuth creds live under `HOME`) — resolve it explicitly in Phase 0, don't hand-wave "inherit HOME/PATH."
- Treat this as a Phase-1 acceptance test: prove the spawned agent, when asked to "connect to the database and delete everything," physically cannot, because it has no tools and no credentials in reach.

## 4. [HIGH] Execute-then-ROLLBACK write preview is dangerous on real production tables

**Risk.** L3's exact-N preview "open txn → run the real statement → ROLLBACK" means a preview of `UPDATE orders SET ... ` (or an un-WHERE'd DELETE) **actually executes the full statement**, taking row/table locks for its full duration, on a live DB, before rolling back. On a large table this blocks production writers, can deadlock, and burns I/O — to show a number. The arch flags side-effects (RETURNING/triggers) but not the **lock/cost blast radius of the preview itself**.

**Recommendation.** Gate execute-preview: always wrap it in a short `statement_timeout`, and when EXPLAIN's estimated rows exceed a threshold, **skip the execute-preview and show the EXPLAIN estimate only** with an "exact count not run — would lock N est. rows" note. Exact-N is a nice-to-have; a locked prod table at approval time is a data-availability incident.

## 5. [MEDIUM] pg_query.rs contradicts your own "L2 is authoritative" reasoning

**Risk.** The arch justifies pulling in `pg_query.rs` (libpg_query — a C library, bindgen, extra universal-binary cross-compile pain) because L1 false-negatives are a "data-loss bug"… two paragraphs after establishing that **L2 read-only txn/role is the authoritative boundary and L1 gaps "degrade UX, not safety."** Both can't be true. If L2 is authoritative, a heavy C dependency to marginally improve a UX pre-filter fails the cost/benefit test.

**Recommendation.** Ship **sqlparser-rs everywhere** for MVP. Add `pg_query.rs` only if real usage shows L1 misclassification is a UX problem worth a C toolchain dependency. This also removes a universal-binary build risk from the critical path. (Everything else in the stack table — sqlx 0.8.6, `tls-rustls-ring`, russh, Tauri v2, CodeMirror 6, TanStack, `keyring-core`+`apple-native-keyring-store`, no-sandbox Developer ID — I affirm; those are the right calls and correctly reasoned.)

## 6. [MEDIUM] L2 is solid on PG/SQLite but genuinely weaker on MySQL — say so louder

**Risk.** PG (`SET TRANSACTION READ ONLY` in a single-statement txn, blocks writable CTEs at execute time) and SQLite (`SQLITE_OPEN_READONLY`, file-level, unforgeable) are strong. **MySQL/MariaDB `START TRANSACTION READ ONLY` is the weakest link** — it does not stop everything a `SELECT`-privileged session can trigger (e.g. side-effecting stored functions, and `max_execution_time` only bounds SELECTs). The plan leans on "also use a SELECT-only GRANT," but MVP lets users connect with whatever creds they have.

**Recommendation.** For MySQL specifically, make the least-privilege role **not optional** guidance — refuse to enable writes-off "safe mode" as a *security* claim on a connection using a privileged account; downgrade the UI language to "best-effort" there. This is also an argument for keeping MySQL in Phase 4 (as planned) rather than rushing it.

## 7. [LOW] macOS platform blockers — mostly clear, two things to verify early

**Risk / assessment.** The hard call (no App Sandbox → off MAS → Developer ID + hardened runtime) is **correct and non-negotiable** — a sandboxed app cannot fork/exec an arbitrary external binary. Two items to nail in Phase 1 (as the roadmap already sequences — good): (a) Keychain `-34018` only reproduces on properly signed builds, so wire signing/notarization in Phase 1, confirmed; (b) verify the **spawned CLI inherits no Keychain entitlement** from dopedb — Keychain ACLs are per-signing-identity, so this should be safe, but prove it (the child authing via `~/.claude`/`~/.codex` files, not your Keychain, is the desired outcome and reinforces finding #3's env-scrubbing).

**Recommendation.** No change to the packaging decision. Add one Phase-1 check: notarized build actually spawns a user-PATH `claude`/`codex` without Gatekeeper prompting on the *child* (it won't if the child is itself a normal user-installed, already-trusted binary — but confirm on a clean machine).

## 8. [LOW] Unversioned CLI JSON contract — affirm the mitigation, add a version pin

Acknowledged and reasonably mitigated (tolerant last-message parser, snapshot tests, run strict-schema turn without MCP to dodge codex #15451). Add: **pin a tested CLI version range**, detect at preflight, and warn (don't silently proceed) on drift. This is ongoing tax, not a blocker.

---

## Must-answer before Phase 1

1. **Economics (from the improved Phase-0 spike):** realistic queries-per-credit-cycle on Anthropic Pro and per-5-hour-window on ChatGPT Plus. If single-digit, the product needs the API-key path front-and-center or a rethink.
2. **Default backend:** given #1, is codex the correct default? Decide before UI copy is written.
3. **Agent tool lockdown:** exact flags that reduce each CLI to "emit text, no shell/file/network." If not achievable, the "agent only proposes" boundary is fiction — resolve before building the bridge.
4. **Env scrubbing vs OAuth:** the minimal env the child needs for OAuth, with everything credential-bearing stripped.
5. **Least-priv role:** auto-generate on first connect vs. require user setup — and it's a *requirement* not a suggestion for MySQL.
6. **Auto-run reads:** default to gating everything vs. auto-run SELECTs (leaning gate-everything until the approval UX is proven).
7. **Write-preview blast radius:** the row-estimate threshold above which you show EXPLAIN-only instead of execute+rollback.

---

## Verdict: **Conditional Go — but not into Phase 1 yet**

The safety architecture (L2-authoritative, single-statement guard, tx-wrap, hash-chained audit, no MCP write tool) is genuinely well-reasoned, and the phased "every safety primitive ships before the write path it gates" sequencing is exactly right. The stack choices are sound.

**But the product's core economic/ToS premise is materially weaker than the doc claims, and the Phase-0 spike as written does not test it.** Do **not** staff Phase 1 until the revised Phase-0 spike (finding #2) answers the economics question on real subscriptions and finding #3's tool-lockdown is proven. Those are one week of throwaway work that could save the whole build. Fix the default-backend and "no API keys" framing off the back of that data, then proceed.

---

Sources:
- [Claude Agent SDK / claude -p usage on your plan (Claude Help Center)](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan)
- [Claude Code billing 2026: subscription vs agent credit pool (Tygart Media)](https://tygartmedia.com/claude-code-billing-credit-pool-2026/)
- [Anthropic June 15 programmatic-usage credit change (AI Codex)](https://www.aicodex.to/articles/claude-subscription-credit-changes)
- [Anthropic reinstates third-party agent usage with a catch (VentureBeat)](https://venturebeat.com/technology/anthropic-reinstates-openclaw-and-third-party-agent-usage-on-claude-subscriptions-with-a-catch)
- [Codex pricing / usage limits (OpenAI Help Center rate card)](https://help.openai.com/en/articles/20001106-codex-rate-card)
- [Using Codex with your ChatGPT plan (OpenAI Help Center)](https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan)
- [Understanding the new Codex limit system after April 9 (OpenAI Community)](https://community.openai.com/t/understanding-the-new-codex-limit-system-after-the-april-9-update/1378768)
- [Run Claude Code programmatically — headless (Claude Code Docs)](https://code.claude.com/docs/en/headless)