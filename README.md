# agent-db

macOS-native database client that lets you operate your databases in natural language by
driving an AI agent CLI you **already subscribe to** (Claude Code / OpenAI Codex) as a backend —
while agent-db owns the connection, the credentials, and a strict safety pipeline:
**read-only by default · human approval gate · full audit log · transactional writes with rollback preview.**

> **Status:** design phase (2026-07-01). No application code yet. Start from the docs below.

## Design docs
- [ARCHITECTURE.md](./ARCHITECTURE.md) — system design, the agent bridge, the 4-layer safety model, tech stack, risks.
- [ROADMAP.md](./ROADMAP.md) — phased build plan (Phase 0 de-risking spike → MVP → v1) + repo layout.
- [DESIGN-REVIEW.md](./DESIGN-REVIEW.md) — adversarial pre-build review. **Read before writing code.**

## Decide before Phase 1 (from the design review)
1. **Economics / default backend.** Research indicates that as of mid-2026 `claude -p` bills against a
   *separate metered Agent-SDK credit pool* (not your interactive subscription), while `codex exec` still
   draws from the ChatGPT subscription window. This weakens the "no API keys / use your subscription" thesis
   for Claude and argues for **codex as the default**. → Verify empirically in the **revised Phase 0 spike**
   on your real subscriptions before committing framing.
2. **Agent tool lockdown.** The spawned CLI is a full agent (shell / filesystem / network). It must be reduced
   to "emit text only" with a **scrubbed environment**, or "the agent only proposes SQL" is not an *enforced*
   boundary — it could read `~/.pgpass`/env DSNs and connect on its own, outside the audit log.
3. **Write-preview blast radius.** execute-then-`ROLLBACK` previews actually run the statement and take locks
   on live tables — cap by EXPLAIN row estimate (show estimate-only above a threshold).
4. Least-privilege DB role (auto-create vs require) · auto-run reads vs gate-everything.

See [DESIGN-REVIEW.md](./DESIGN-REVIEW.md) → "Must-answer before Phase 1" for the full list and the
**Conditional Go** verdict (do the revised Phase 0 spike before staffing Phase 1).
