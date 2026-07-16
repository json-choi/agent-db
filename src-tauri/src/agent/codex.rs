//! Codex CLI adapter: builds the `codex exec` command for one turn and parses its
//! `--json` JSONL stdout. No config file is needed — the MCP server is wired in with
//! `-c mcp_servers.<name>.*` overrides, and tool access is capped by the read-only
//! sandbox policy rather than an allow/deny list (Codex has none for MCP tools).
//!
//! Flag order verified against `codex exec --help` / `codex exec resume --help`
//! (codex-cli 0.142.5): `-s/--sandbox` and `-c` are options of `codex exec` itself,
//! not of the `resume` subcommand, so they must come BEFORE `resume <id>` — clap
//! rejects `codex exec resume <id> -s read-only` outright. Verified live (fake
//! resume id, deliberately unreachable MCP url) that flags placed before `resume`
//! really are applied to the resumed run, not just accepted syntactically and
//! discarded: the process got past config loading and made it all the way to
//! `Error: thread/resume: ... no rollout found for thread id ...` — i.e. it failed
//! on the fake id, not on the config.
//!
//! The override server name is `dopedb-chat`, deliberately NOT `dopedb`: DopeDB's
//! existing Settings > MCP "connect" flow (`mcp::connect::connect`) already runs
//! `codex mcp add dopedb -- <bridge>`, which persists a *stdio*-type
//! `[mcp_servers.dopedb]` table in `~/.codex/config.toml`. Verified live that
//! reusing that name collides: `-c mcp_servers.dopedb.url=...` merges onto the
//! persisted stdio entry and codex refuses to start with "Error loading
//! config.toml: url is not supported for stdio in `mcp_servers.dopedb`" — so every
//! chat turn would fail for any user who had already connected Codex via Settings >
//! MCP. A distinct name sidesteps the collision entirely.

use tokio::process::Command;

use super::ChatSignal;
use crate::error::{AppError, AppResult};
use crate::mcp::connect::{quiet_command, which};

/// Build the `codex exec` command for one turn. `resume` is the CLI's own thread id
/// from a prior turn (`None` starts a fresh conversation); `token` authenticates the
/// child against DopeDB's own MCP server and is passed as an env var only. `model`/
/// `effort` are the user's per-provider picks from the chat header (`None` = CLI
/// default). Like `-s`/`-c` above, both must precede the `resume` subcommand token —
/// `codex exec resume <id> -m …` is rejected outright by clap ("unexpected argument"),
/// verified live.
pub(super) fn command(
    message: &str,
    resume: Option<&str>,
    token: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> AppResult<Command> {
    let bin = which("codex").ok_or_else(|| AppError::Agent("Codex (`codex`) not found".into()))?;
    let mut cmd: Command = quiet_command(&bin).into();
    cmd.args(build_args(message, resume, model, effort))
        .env("DOPEDB_MCP_TOKEN", token);
    Ok(cmd)
}

/// The full argv (after the binary) for one turn. Pure — no PATH lookups — so the
/// argument-order regression tests run on machines without a `codex` install (CI
/// runners have none; the old tests that went through `command()` failed there).
fn build_args(
    message: &str,
    resume: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        // Read-only sandbox: the model can't touch the filesystem/shell even if a
        // prompt tries to talk it into something outside the MCP tools.
        "-s".into(),
        "read-only".into(),
        "-c".into(),
        format!("mcp_servers.dopedb-chat.url={}", crate::mcp::mcp_url()),
        "-c".into(),
        "mcp_servers.dopedb-chat.bearer_token_env_var=DOPEDB_MCP_TOKEN".into(),
    ];
    if let Some(m) = model {
        args.extend(["-m".into(), m.into()]);
    }
    if let Some(e) = effort {
        args.extend(["-c".into(), format!("model_reasoning_effort={e}")]);
    }
    if let Some(id) = resume {
        args.extend(["resume".into(), id.into()]);
    }
    // `--` stops the message from being absorbed as the value of a preceding `-c`
    // (or otherwise reinterpreted as an option) — verified on a real `codex` binary:
    // without it, a message starting with `-` overwrites the last `-c` flag's value
    // and the actual prompt never reaches the model.
    args.extend(["--".into(), message.into()]);
    args
}

/// Parse one line of `codex exec --json` JSONL into zero or more chat signals. Every
/// field access is `Option`-based — an unrecognized/changed schema yields no signals
/// rather than a panic.
pub(super) fn parse_line(v: &serde_json::Value) -> Vec<ChatSignal> {
    let mut out = Vec::new();
    match v.get("type").and_then(|t| t.as_str()) {
        Some("thread.started") => {
            if let Some(id) = v.get("thread_id").and_then(|s| s.as_str()) {
                out.push(ChatSignal::SessionId(id.to_string()));
            }
        }
        Some("item.completed") => {
            if v.pointer("/item/type").and_then(|t| t.as_str()) == Some("agent_message") {
                if let Some(text) = v.pointer("/item/text").and_then(|t| t.as_str()) {
                    out.push(ChatSignal::Text(text.to_string()));
                }
            }
        }
        Some("turn.failed") | Some("error") => {
            // `error` carries the message at the top level, `turn.failed` nests it
            // under `error.message` — verified against a live codex-cli run.
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .or_else(|| v.pointer("/error/message").and_then(|m| m.as_str()))
                .unwrap_or("Codex reported an error")
                .to_string();
            out.push(ChatSignal::TurnFailed(msg));
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(signals: &[ChatSignal]) -> Vec<&str> {
        signals
            .iter()
            .filter_map(|s| match s {
                ChatSignal::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    fn session_ids(signals: &[ChatSignal]) -> Vec<&str> {
        signals
            .iter()
            .filter_map(|s| match s {
                ChatSignal::SessionId(id) => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn thread_started_yields_the_thread_id() {
        let line: serde_json::Value =
            serde_json::from_str(r#"{"type":"thread.started","thread_id":"thr-1"}"#).unwrap();
        assert_eq!(session_ids(&parse_line(&line)), vec!["thr-1"]);
    }

    #[test]
    fn agent_message_item_completed_yields_its_text() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"hello"}}"#,
        )
        .unwrap();
        assert_eq!(texts(&parse_line(&line)), vec!["hello"]);
    }

    #[test]
    fn other_item_completed_kinds_yield_no_text() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"item.completed","item":{"type":"command_execution","text":"ls"}}"#,
        )
        .unwrap();
        assert!(parse_line(&line).is_empty());
    }

    #[test]
    fn turn_failed_yields_a_turn_failed_signal() {
        let line: serde_json::Value =
            serde_json::from_str(r#"{"type":"turn.failed","message":"boom"}"#).unwrap();
        let signals = parse_line(&line);
        assert!(signals
            .iter()
            .any(|s| matches!(s, ChatSignal::TurnFailed(msg) if msg == "boom")));
    }

    #[test]
    fn turn_failed_reads_the_nested_error_message() {
        // Real codex-cli shape: turn.failed nests the message under `error.message`.
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"turn.failed","error":{"message":"model gone"}}"#,
        )
        .unwrap();
        let signals = parse_line(&line);
        assert!(signals
            .iter()
            .any(|s| matches!(s, ChatSignal::TurnFailed(msg) if msg == "model gone")));
    }

    #[test]
    fn error_event_yields_a_turn_failed_signal() {
        let line: serde_json::Value =
            serde_json::from_str(r#"{"type":"error","message":"nope"}"#).unwrap();
        let signals = parse_line(&line);
        assert!(signals
            .iter()
            .any(|s| matches!(s, ChatSignal::TurnFailed(msg) if msg == "nope")));
    }

    /// `-m`/`-c model_reasoning_effort=…` must land before the `resume` subcommand
    /// token — clap rejects them after `resume` outright ("unexpected argument"),
    /// verified live. This is a regression that has already happened once, so it gets
    /// its own test.
    #[test]
    fn model_and_effort_flags_come_before_the_resume_subcommand() {
        let args = build_args("hi", Some("thr-1"), Some("o3"), Some("high"));
        let resume_pos = args.iter().position(|a| a == "resume").expect("resume present");
        let m_pos = args.iter().position(|a| a == "-m").expect("-m present");
        assert!(m_pos < resume_pos, "-m must precede resume");
        assert_eq!(args[m_pos + 1], "o3");
        let effort_pos = args
            .iter()
            .position(|a| a == "model_reasoning_effort=high")
            .expect("model_reasoning_effort=high present");
        assert!(effort_pos < resume_pos, "-c model_reasoning_effort must precede resume");
        assert_eq!(args[effort_pos - 1], "-c");
    }

    /// No model/effort selected: no flags added, existing behavior unchanged.
    #[test]
    fn no_model_or_effort_omits_both_flags() {
        let args = build_args("hi", None, None, None);
        assert!(!args.iter().any(|a| a == "-m"));
        assert!(!args.iter().any(|a| a.starts_with("model_reasoning_effort=")));
    }
}
