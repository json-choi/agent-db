# Phase 0 spike — minimal plumbing verification (results)

**Date:** 2026-07-01 · **Machine:** user's macOS · **CLIs:** `claude` 2.1.197, `codex-cli` 0.142.5

Scope: the *minimal* subset the user approved — prove **(a)** a subscribed CLI returns parseable SQL
non-interactively with **no API key**, and **(b)** a read-only DB session rejects writes. Full
economics-to-exhaustion and agent tool-lockdown were **deferred** (see below).

## Auth context (the whole point)
`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `CLAUDE_API_KEY` / `CLAUDE_CODE_OAUTH_TOKEN` were all **unset**;
`codex login status` → **"Logged in using ChatGPT"**. Both calls below ran purely on the user's
interactive subscription/OAuth — exactly the product thesis.

## What was proven
| Check | Result |
|---|---|
| (a) Claude NL→SQL, no API key | ✅ PASS — `SELECT * FROM orders ORDER BY created_at DESC LIMIT 5;` |
| (a) Codex NL→SQL, no API key | ✅ PASS — identical SQL |
| End-to-end: Claude's SQL runs read-only, returns the correct 5 most-recent rows | ✅ PASS |
| (b) read-only SQLite session rejects a `DELETE` | ✅ PASS — `attempt to write a readonly database (8)` |

## Working invocations
```sh
# Claude (design default)
cat prompt.txt | claude -p --output-format json --model sonnet \
  --append-system-prompt "Output ONLY one SQL statement. No prose. No markdown code fences."
#   -> SQL in .result (clean, no fences); meta in .total_cost_usd / .usage

# Codex (alternate, behind the same AgentBackend trait)
cat prompt.txt | codex exec --json -s read-only --skip-git-repo-check -o last.txt -
#   -> final SQL written to last.txt; token usage in the --json JSONL events
```

## Economics data point (informs DESIGN-REVIEW finding #1)
| CLI | input tok/turn | output | reported cost | billing model |
|---|---|---|---|---|
| Claude (sonnet) | 7,902 | 32 | **$0.067** | metered vs Agent-SDK credit pool → **~300 turns / $20** |
| Codex | 16,797 | 88 | (window-based) | ChatGPT **5-hour message window** |

Input is dominated by **each CLI's own system-prompt overhead** (~8k Claude / ~17k Codex), **not** our
tiny schema — real schemas add on top. Claude's ~$0.067/turn floor confirms the review's economic-ceiling
concern is real (though ~300 turns/$20, not single-digit). Codex's message-window model looks friendlier
to the "use your subscription" pitch → supports the review's suggestion to consider **codex as default**.

## Deferred (NOT covered by this minimal pass — do before Phase 1)
- **Agent tool-lockdown (review #3):** we ran a pure-translation prompt; we did **not** prove the spawned
  CLI can be reduced to "text only / no shell/file/network" with a scrubbed env. This is the load-bearing
  safety assumption ("the agent only proposes SQL") — must be proven next.
- **Full economics-to-exhaustion:** queries per credit-cycle / per 5-hour window, and the exact
  machine-readable refusal payload to render in the UI.
- **Model-emits-write → blocked by L2 end-to-end:** we proved L2 with a hardcoded write, which is
  sufficient for the plumbing thesis but not the full agent→gate loop.
- **Robustness:** process-group kill + hard timeout on a hung child; startup preflight auth check.

## Read
Core thesis is mechanically **sound**: both subscribed CLIs return clean, parseable SQL with no API key,
and the read-only DB boundary holds. Next Phase-0 items before staffing Phase 1: **agent tool-lockdown +
env scrubbing**, and a **default-backend decision** (codex's window billing vs Claude's metered pool).
