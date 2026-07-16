//! In-app agent chat: spawns the `claude`/`codex` CLI the user has already signed into
//! with their own subscription, turn by turn, and points it at DopeDB's own MCP server
//! (`crate::mcp`) with an inline, single-run config. Credentials are never touched —
//! this module only runs a binary the user has already logged into. There is no
//! long-lived process: each turn spawns a fresh CLI and it exits when the turn ends.
//! Both CLIs can resume a prior turn's session via a `resume`-style flag, so a
//! multi-turn conversation works without a resident process + stdin streaming.

pub mod claude;
pub mod codex;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncBufReadExt;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::executor::cancel;
use crate::mcp::connect;
use crate::store::Store;

/// Which CLI a chat turn runs through. Kebab-case on the wire (`"claude"`/`"codex"`)
/// to match the frontend's `AgentProvider` union. Named `AgentProvider` (not `Provider`)
/// because `model::Provider` already names the DB-hosting provider (Neon/PlanetScale/…).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentProvider {
    Claude,
    Codex,
}

/// Installed/authenticated status for one CLI, used to gate the chat UI before it
/// lets the user send anything.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInfo {
    pub id: AgentProvider,
    pub name: String,
    pub installed: bool,
    pub authenticated: bool,
    /// Claude's `auth status` reports how the user is signed in (e.g. `"claude.ai"`
    /// for a subscription login); Codex has no equivalent field, so `None`.
    pub auth_method: Option<String>,
    pub note: String,
}

/// One persisted chat thread (mirrors `agent_chat_threads`). `cli_session_id` is the
/// underlying CLI's own resume token; `model`/`effort` are the values USED by the most
/// recent turn, so the frontend picker can seed itself from them on thread switch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatThread {
    pub id: Uuid,
    pub provider: AgentProvider,
    pub title: String,
    pub cli_session_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One persisted chat message (mirrors `agent_chat_messages`). `role` is `"user"` or
/// `"assistant"`; `error` is set only on a failed assistant turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageRecord {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub role: String,
    pub text: String,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One selectable model from a provider's own catalog. `efforts` is the reasoning-
/// level list to offer for THIS model; `default_effort` seeds the effort picker
/// (`None` = the CLI has no notion of an explicit default — "default" still shows
/// as a valid picker choice, it just passes no `--effort`/`-c` flag through).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModel {
    pub id: String,
    pub name: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

/// App-wide chat concurrency gate. Session resume now lives per-thread in the store
/// (`ChatThread::cli_session_id`), so this slot only serializes turns: `turn_lock`
/// makes a second `send_turn` that races in wait for the first to finish, and
/// `active_turn` is a separate, always-available lock so the app-exit hook can check
/// "is a turn running" without waiting on a turn that might still be mid-flight.
pub struct ChatSlot {
    turn_lock: AsyncMutex<()>,
    active_turn: std::sync::Mutex<Option<Uuid>>,
}

/// Shared handle stored in `AppState.chat`.
pub type ChatState = Arc<ChatSlot>;

/// Fresh, empty chat memory — no active turn yet.
pub fn chat_state() -> ChatState {
    Arc::new(ChatSlot {
        turn_lock: AsyncMutex::new(()),
        active_turn: std::sync::Mutex::new(None),
    })
}

impl ChatSlot {
    /// Turn id of whichever chat turn is currently spawning/running, if any. The
    /// Tauri exit hook reads this to cancel an in-flight turn so its child process
    /// is never orphaned past app shutdown.
    pub fn active_turn(&self) -> Option<Uuid> {
        *self.active_turn.lock().unwrap()
    }

    /// Waits (bounded by `timeout`) for the active turn to clear. Used by the app-exit
    /// hook: it signals cancellation and then needs to know the turn's task actually
    /// observed it and dropped the child (which is what `kill_on_drop` fires on) before
    /// letting the process exit — otherwise the signal and the exit race each other.
    pub async fn wait_idle(&self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.active_turn().is_some() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Clears `active_turn` on every exit path out of [`send_turn`] (success, error, `?`
/// early-return, or cancellation) — a plain Drop guard so no path can forget to reset it.
struct ActiveTurnGuard<'a>(&'a ChatSlot);

impl Drop for ActiveTurnGuard<'_> {
    fn drop(&mut self) {
        *self.0.active_turn.lock().unwrap() = None;
    }
}

/// Wall-clock ceiling for one chat turn — generous relative to a plain DB query
/// ([`crate::executor::cancel::QUERY_TIMEOUT`] is 300s) since a turn may run several
/// MCP tool calls back to back.
const CHAT_TURN_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatEventPayload<'a> {
    turn_id: Uuid,
    thread_id: Uuid,
    text_chunk: &'a str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatDonePayload<'a> {
    turn_id: Uuid,
    thread_id: Uuid,
    ok: bool,
    error: Option<&'a str>,
}

fn emit_event(app: &AppHandle, turn_id: Uuid, thread_id: Uuid, text_chunk: &str) {
    if let Err(e) = app.emit(
        "agent:chat_event",
        ChatEventPayload { turn_id, thread_id, text_chunk },
    ) {
        tracing::warn!("failed to emit agent:chat_event: {e}");
    }
}

fn emit_done(app: &AppHandle, turn_id: Uuid, thread_id: Uuid, ok: bool, error: Option<&str>) {
    if let Err(e) = app.emit(
        "agent:chat_done",
        ChatDonePayload { turn_id, thread_id, ok, error },
    ) {
        tracing::warn!("failed to emit agent:chat_done: {e}");
    }
}

/// First 50 non-newline characters of `message`, trimmed — used to derive a thread's
/// title from its opening user message when the thread has none yet.
fn thread_title_from(message: &str) -> String {
    message
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .take(50)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Neutral working directory for the spawned CLI. This feature does no filesystem
/// work, so hand the child an empty directory (created on demand) instead of the
/// Tauri process's real cwd (a macOS app bundle's is `Contents/MacOS`) — if a tool
/// restriction flag ever has a hole, there's nothing nearby to find.
fn agent_workdir() -> std::path::PathBuf {
    let dir = dirs::data_dir().unwrap_or_default().join("dopedb").join("agent-workdir");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A signal extracted from one parsed JSONL line of CLI output.
enum ChatSignal {
    /// A chunk of assistant text to append to the current turn's message.
    Text(String),
    /// The CLI's own session id — remembered so the next turn can resume it.
    SessionId(String),
    /// The CLI reported an error for this turn (distinct from a non-JSON line or a
    /// non-zero exit with no structured error — see `send_turn`'s stderr fallback).
    TurnFailed(String),
}

/// Best-effort short tail of stderr, used as the error message when a turn's process
/// exits non-zero without ever emitting a structured `TurnFailed` signal. Capped so a
/// chatty CLI can't balloon memory before the child is torn down.
async fn read_tail(stderr: tokio::process::ChildStderr) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let _ = stderr.take(4096).read_to_end(&mut buf).await;
    let text = String::from_utf8_lossy(&buf).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Run one chat turn against `thread_id`: load the thread's provider + resumable CLI
/// session, insert the user message immediately, spawn the CLI, stream its stdout as
/// `agent:chat_event`s, and on completion (success, error, cancel, or timeout) persist
/// the assistant's reply, advance the thread's session/model/effort/title, and emit
/// exactly one `agent:chat_done`. Holding `state.turn_lock` for the whole turn means a
/// second `send_turn` that races in (e.g. a double-click) waits for this one to finish
/// instead of needing its own queue.
#[allow(clippy::too_many_arguments)]
pub async fn send_turn(
    app: AppHandle,
    state: ChatState,
    store: Store,
    mcp_token: String,
    thread_id: Uuid,
    message: String,
    turn_id: Uuid,
    model: Option<String>,
    effort: Option<String>,
) -> AppResult<()> {
    let _turn_guard = state.turn_lock.lock().await;

    *state.active_turn.lock().unwrap() = Some(turn_id);
    let _clear_active = ActiveTurnGuard(&state);

    let thread = store.get_chat_thread(thread_id).await?;
    let provider = thread.provider;
    let resume = thread.cli_session_id.clone();

    // Persisted before the CLI ever spawns, so the message survives even a turn that
    // fails to start (bad binary, etc.).
    store.insert_chat_message(thread_id, "user", &message, None).await?;

    let cmd_result = match provider {
        AgentProvider::Claude => {
            claude::command(&message, resume.as_deref(), &mcp_token, model.as_deref(), effort.as_deref())
        }
        AgentProvider::Codex => {
            codex::command(&message, resume.as_deref(), &mcp_token, model.as_deref(), effort.as_deref())
        }
    };

    // `cmd_result`'s Err (bad binary, config dir unwritable, etc.) is folded into
    // `outcome` below rather than `?`-propagated here, so a turn that fails before it
    // ever spawns still reaches the outcome-handling block: the already-persisted user
    // message gets a matching assistant/error row and the thread's title/updated_at
    // are still set, instead of the turn silently vanishing from the transcript.
    let outcome: AppResult<(Option<String>, bool, Option<String>, String)> = match cmd_result {
        Ok(mut cmd) => {
            cmd.current_dir(agent_workdir())
                .kill_on_drop(true) // a cancelled/timed-out/dropped future kills the real child too
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let run = async {
                let mut child = cmd.spawn().map_err(AppError::Io)?;
                let stdout = child.stdout.take().expect("piped");
                let stderr = child.stderr.take().expect("piped");
                let mut out_lines = tokio::io::BufReader::new(stdout).lines();
                let mut new_session_id = resume.clone();
                let mut assembled_error: Option<String> = None;
                let mut assembled_text = String::new();

                while let Some(line) = out_lines.next_line().await.map_err(AppError::Io)? {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value: serde_json::Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        // The CLI is asked for JSONL output, but a stray banner/warning line on
                        // stdout is possible. Drop just that line (logged, not silent) instead
                        // of failing the whole turn over it.
                        Err(e) => {
                            tracing::warn!("chat: non-JSON line from {provider:?}: {e} ({line})");
                            continue;
                        }
                    };
                    let signals = match provider {
                        AgentProvider::Claude => claude::parse_line(&value),
                        AgentProvider::Codex => codex::parse_line(&value),
                    };
                    for signal in signals {
                        match signal {
                            ChatSignal::Text(chunk) => {
                                emit_event(&app, turn_id, thread_id, &chunk);
                                assembled_text.push_str(&chunk);
                            }
                            ChatSignal::SessionId(id) => new_session_id = Some(id),
                            ChatSignal::TurnFailed(msg) => assembled_error = Some(msg),
                        }
                    }
                }
                let stderr_tail = read_tail(stderr).await;
                let status = child.wait().await.map_err(AppError::Io)?;
                // A CLI can report a turn failure via structured JSONL (`TurnFailed`) while
                // still exiting 0 — the exit code alone isn't a reliable success signal, so an
                // explicit failure signal always overrides it.
                let ok = status.success() && assembled_error.is_none();
                Ok::<_, AppError>((new_session_id, ok, assembled_error.or(stderr_tail), assembled_text))
            };

            cancel::guard(Some(turn_id), CHAT_TURN_TIMEOUT, run).await
        }
        Err(e) => Err(e),
    };
    // Even a cancelled/errored-before-any-signal turn still gets an assistant row +
    // thread update — a failed turn should never vanish from the transcript, and the
    // (possibly still-`None`) session id must round-trip so a later retry can resume.
    let (session_id, ok, error, assembled_text) = match &outcome {
        Ok((session_id, ok, error, text)) => (session_id.clone(), *ok, error.clone(), text.clone()),
        Err(e) => (resume.clone(), false, Some(e.to_string()), String::new()),
    };

    if let Err(e) = store
        .insert_chat_message(thread_id, "assistant", &assembled_text, error.as_deref())
        .await
    {
        tracing::error!("chat: failed to persist assistant message for thread {thread_id}: {e}");
    }
    if let Err(e) = store
        .finish_chat_turn(thread_id, session_id, model, effort, &thread_title_from(&message))
        .await
    {
        tracing::error!("chat: failed to update thread {thread_id} after turn: {e}");
    }

    emit_done(&app, turn_id, thread_id, ok, error.as_deref());
    match outcome {
        Ok((_, true, _, _)) => Ok(()),
        Ok((_, false, error, _)) => Err(AppError::Agent(
            error.unwrap_or_else(|| format!("{provider:?} exited with a non-zero status")),
        )),
        Err(e) => Err(e),
    }
}

/// Reasoning levels Claude Code accepts, common to every model.
const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Static fallback catalog: Claude Code has no `debug models`-style catalog command.
// ponytail: hardcoded per the confirmed model-catalog contract — swap for a real
// catalog command (parsed like `codex_models` below) the moment claude-cli grows one.
fn claude_models() -> Vec<AgentModel> {
    [("fable", "Fable"), ("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
        .into_iter()
        .map(|(id, name)| AgentModel {
            id: id.into(),
            name: name.into(),
            efforts: CLAUDE_EFFORTS.iter().map(|s| s.to_string()).collect(),
            default_effort: None,
        })
        .collect()
}

/// Codex's own model catalog via `codex debug models` — a local, offline render (no
/// network call, no cost), so this can run every time the chat header opens. Only
/// `visibility == "list"` entries are user-selectable; the rest (e.g. an internal
/// `codex-auto-review` entry) are Codex-internal and hidden from `codex --help` too.
/// Sorted by ascending `priority` (lower = more prominent in Codex's own UI).
fn codex_models() -> AppResult<Vec<AgentModel>> {
    let bin = connect::which("codex").ok_or_else(|| AppError::Agent("Codex (`codex`) not found".into()))?;
    let out = connect::run(&bin, &["debug", "models"]).map_err(AppError::Agent)?;
    let value: serde_json::Value = serde_json::from_str(&out)
        .map_err(|e| AppError::Agent(format!("codex debug models: unparseable JSON ({e})")))?;
    let models = value
        .get("models")
        .and_then(|m| m.as_array())
        .ok_or_else(|| AppError::Agent("codex debug models: response has no 'models' array".into()))?;

    let mut listed: Vec<(i64, AgentModel)> = models
        .iter()
        .filter(|m| m.get("visibility").and_then(|v| v.as_str()) == Some("list"))
        .filter_map(|m| {
            let slug = m.get("slug")?.as_str()?.to_string();
            let name = m
                .get("display_name")
                .and_then(|d| d.as_str())
                .unwrap_or(&slug)
                .to_string();
            let priority = m.get("priority").and_then(|p| p.as_i64()).unwrap_or(i64::MAX);
            let efforts = m
                .get("supported_reasoning_levels")
                .and_then(|l| l.as_array())
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|l| l.get("effort").and_then(|e| e.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let default_effort =
                m.get("default_reasoning_level").and_then(|d| d.as_str()).map(String::from);
            Some((priority, AgentModel { id: slug, name, efforts, default_effort }))
        })
        .collect();
    listed.sort_by_key(|(priority, _)| *priority);
    Ok(listed.into_iter().map(|(_, m)| m).collect())
}

/// List the models `provider`'s CLI can run a turn against. Codex reads its own live
/// (offline) catalog; Claude Code falls back to the static list above. Errors
/// propagate rather than degrading to an empty list — a broken catalog read should be
/// visible, not silently presented as "this CLI has zero models".
pub fn list_models(provider: AgentProvider) -> AppResult<Vec<AgentModel>> {
    match provider {
        AgentProvider::Claude => Ok(claude_models()),
        AgentProvider::Codex => codex_models(),
    }
}

/// Installed + subscription-login status for both supported CLIs. Deliberately
/// separate from `mcp::connect::detect()` (which asks "is dopedb registered in this
/// platform's MCP config") — this answers a different question, so the chat gate and
/// the Settings > MCP screen never move each other's state around.
pub fn detect_clis() -> Vec<CliInfo> {
    vec![detect_claude(), detect_codex()]
}

fn detect_claude() -> CliInfo {
    let bin = connect::which("claude");
    // A bare `which` hit isn't enough — on Windows a Claude Desktop GUI install is
    // known to shadow the `claude` CLI name on PATH, so confirm it's actually the CLI.
    let installed = bin
        .as_ref()
        .is_some_and(|b| connect::run(b, &["--version"]).is_ok_and(|out| out.contains("Claude Code")));

    let (authenticated, auth_method) = if installed {
        bin.as_ref()
            .and_then(|b| connect::run(b, &["auth", "status"]).ok())
            .and_then(|out| serde_json::from_str::<serde_json::Value>(&out).ok())
            .filter(|v| v.get("loggedIn").and_then(|b| b.as_bool()).unwrap_or(false))
            .map(|v| (true, v.get("authMethod").and_then(|m| m.as_str()).map(String::from)))
            .unwrap_or((false, None))
    } else {
        (false, None)
    };

    CliInfo {
        id: AgentProvider::Claude,
        name: "Claude Code".into(),
        installed,
        authenticated,
        auth_method,
        note: "Uses your Claude Pro/Max subscription login.".into(),
    }
}

fn detect_codex() -> CliInfo {
    let bin = connect::which("codex");
    let installed = bin
        .as_ref()
        .is_some_and(|b| connect::run(b, &["--version"]).is_ok_and(|out| out.contains("codex-cli")));
    let authenticated =
        installed && bin.as_ref().is_some_and(|b| connect::run(b, &["login", "status"]).is_ok());

    CliInfo {
        id: AgentProvider::Codex,
        name: "Codex CLI".into(),
        installed,
        authenticated,
        auth_method: None,
        note: "Uses your ChatGPT Plus/Pro subscription login.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_idle_returns_promptly_once_the_turn_clears() {
        let slot = chat_state();
        *slot.active_turn.lock().unwrap() = Some(Uuid::new_v4());
        let clearer = slot.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            *clearer.active_turn.lock().unwrap() = None;
        });

        let start = tokio::time::Instant::now();
        slot.wait_idle(Duration::from_secs(5)).await;
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "should return as soon as active_turn clears, not wait out the full bound"
        );
    }

    #[tokio::test]
    async fn wait_idle_gives_up_after_the_timeout_if_the_turn_never_clears() {
        let slot = chat_state();
        *slot.active_turn.lock().unwrap() = Some(Uuid::new_v4());

        let start = tokio::time::Instant::now();
        slot.wait_idle(Duration::from_millis(50)).await;
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert!(slot.active_turn().is_some(), "bounded wait must not block forever");
    }
}
