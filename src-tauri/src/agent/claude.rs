//! Claude Code CLI adapter: builds the `claude -p … --output-format stream-json`
//! command for one turn and parses its JSONL stdout. Session resume, the inline MCP
//! config, and the tool-restriction flags all live here — `agent::mod` only knows
//! about the CLI-agnostic [`super::ChatSignal`] shape.

use std::path::PathBuf;

use tokio::process::Command;

use super::ChatSignal;
use crate::error::{AppError, AppResult};
use crate::mcp::connect::{quiet_command, which};

/// The seven read-only MCP tools DopeDB's server exposes (see `mcp::tools`). Listed
/// explicitly rather than a `mcp__dopedb__*` wildcard — Claude Code does not expand
/// wildcards in `--allowedTools`, so a wildcard silently allows nothing. Kept in sync
/// with `mcp::tools::DbTools`'s actual tool catalog by
/// `mcp::tools::tests::claude_allowed_tools_matches_the_mcp_tool_catalog`.
pub(crate) const ALLOWED_TOOLS: [&str; 7] = [
    "mcp__dopedb__list_connections",
    "mcp__dopedb__list_tables",
    "mcp__dopedb__describe_table",
    "mcp__dopedb__plan_query",
    "mcp__dopedb__run_query",
    "mcp__dopedb__run_document_query",
    "mcp__dopedb__create_dashboard",
];

/// Every built-in tool that could touch the filesystem, shell, or network outside of
/// DopeDB's own MCP server. Chat only ever needs the MCP tools above.
const DISALLOWED_TOOLS: [&str; 10] = [
    "Bash",
    "Edit",
    "Write",
    "Read",
    "Glob",
    "Grep",
    "WebSearch",
    "WebFetch",
    "NotebookEdit",
    "Task",
];

/// A static, single-run MCP config file. It holds no secret: the real bearer token is
/// referenced as the `${DOPEDB_MCP_TOKEN}` placeholder (Claude Code expands `${VAR}`
/// in `--mcp-config` from the child's own environment) and passed only as an env var
/// to the spawned process, so the token is never written to disk in plaintext.
fn mcp_config_path() -> AppResult<PathBuf> {
    let dir = dirs::data_dir().ok_or_else(|| AppError::Config("no data dir".into()))?.join("dopedb");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("agent-mcp-config.json");
    if !path.exists() {
        let body = serde_json::json!({
            "mcpServers": {
                "dopedb": {
                    "type": "http",
                    "url": crate::mcp::mcp_url(),
                    "headers": { "Authorization": "Bearer ${DOPEDB_MCP_TOKEN}" }
                }
            }
        })
        .to_string();
        crate::mcp::write_private(&path, body.as_bytes()); // reused: 0600 / Windows ACL
    }
    Ok(path)
}

/// Build the `claude` command for one turn. `resume` is the CLI's own session id from
/// a prior turn (`None` starts a fresh conversation); `token` authenticates the child
/// against DopeDB's own MCP server and is passed as an env var only. `model`/`effort`
/// are the user's per-provider picks from the chat header (`None` = CLI default).
pub(super) fn command(
    message: &str,
    resume: Option<&str>,
    token: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> AppResult<Command> {
    let bin = which("claude").ok_or_else(|| AppError::Agent("Claude Code (`claude`) not found".into()))?;
    let cfg = mcp_config_path()?;
    let mut cmd: Command = quiet_command(&bin).into();
    cmd.args(build_args(message, resume, &cfg.to_string_lossy(), model, effort))
        .env("DOPEDB_MCP_TOKEN", token);
    Ok(cmd)
}

/// The full argv (after the binary) for one turn. Pure — no filesystem or PATH lookups —
/// so the argument-order regression tests run on machines without a `claude` install
/// (CI runners have none; the old tests that went through `command()` failed there).
fn build_args(
    message: &str,
    resume: Option<&str>,
    mcp_config: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"]
        .map(String::from)
        .into();
    args.extend(["--mcp-config".into(), mcp_config.into(), "--strict-mcp-config".into()]);
    args.push("--allowedTools".into());
    args.extend(ALLOWED_TOOLS.map(String::from));
    args.push("--disallowedTools".into());
    args.extend(DISALLOWED_TOOLS.map(String::from));
    if let Some(id) = resume {
        args.extend(["--resume".into(), id.into()]);
    }
    if let Some(m) = model {
        args.extend(["--model".into(), m.into()]);
    }
    if let Some(e) = effort {
        args.extend(["--effort".into(), e.into()]);
    }
    // `--` must come after EVERY option and right before the message: it marks the end
    // of options, so anything following it — including our own flags — would be
    // swallowed into the prompt. It exists so a message starting with `-` (e.g.
    // "-1 rows are wrong") can't be reinterpreted as an option.
    args.extend(["--".into(), message.into()]);
    args
}

/// Parse one line of `claude --output-format stream-json` JSONL into zero or more
/// chat signals. Every field access is `Option`-based — an unrecognized/changed
/// schema yields no signals rather than a panic.
pub(super) fn parse_line(v: &serde_json::Value) -> Vec<ChatSignal> {
    let mut out = Vec::new();
    match v.get("type").and_then(|t| t.as_str()) {
        Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("init") => {
            if let Some(id) = v.get("session_id").and_then(|s| s.as_str()) {
                out.push(ChatSignal::SessionId(id.to_string()));
            }
        }
        Some("assistant") => {
            if let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for block in blocks {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        out.push(ChatSignal::Text(text.to_string()));
                    }
                }
            }
        }
        Some("result") => {
            if let Some(id) = v.get("session_id").and_then(|s| s.as_str()) {
                out.push(ChatSignal::SessionId(id.to_string()));
            }
            let is_error = v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
            if is_error {
                let msg = v
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("Claude Code reported an error")
                    .to_string();
                out.push(ChatSignal::TurnFailed(msg));
            }
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
    fn system_init_yields_the_session_id() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"system","subtype":"init","session_id":"sess-1"}"#,
        )
        .unwrap();
        assert_eq!(session_ids(&parse_line(&line)), vec!["sess-1"]);
    }

    #[test]
    fn assistant_message_yields_its_text_blocks() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
        )
        .unwrap();
        assert_eq!(texts(&parse_line(&line)), vec!["hello"]);
    }

    #[test]
    fn result_with_is_error_yields_a_turn_failed_signal() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"result","session_id":"sess-1","is_error":true,"result":"boom"}"#,
        )
        .unwrap();
        let signals = parse_line(&line);
        assert_eq!(session_ids(&signals), vec!["sess-1"]);
        assert!(signals
            .iter()
            .any(|s| matches!(s, ChatSignal::TurnFailed(msg) if msg == "boom")));
    }

    #[test]
    fn result_without_is_error_yields_no_failure() {
        let line: serde_json::Value =
            serde_json::from_str(r#"{"type":"result","session_id":"sess-1","is_error":false}"#)
                .unwrap();
        let signals = parse_line(&line);
        assert!(!signals.iter().any(|s| matches!(s, ChatSignal::TurnFailed(_))));
    }

    #[test]
    fn unknown_type_yields_no_signals() {
        let line: serde_json::Value = serde_json::from_str(r#"{"type":"something_new"}"#).unwrap();
        assert!(parse_line(&line).is_empty());
    }

    /// `--model`/`--effort` must land before the `--` message separator — anything
    /// after `--` is absorbed into the prompt instead of being parsed as a flag. This
    /// is a regression that has already happened once, so it gets its own test.
    #[test]
    fn model_and_effort_flags_come_before_the_message_separator() {
        let args = build_args("hi", None, "/cfg.json", Some("opus"), Some("high"));
        let dash_dash = args.iter().position(|a| a == "--").expect("-- present");
        let model_pos = args.iter().position(|a| a == "--model").expect("--model present");
        let effort_pos = args.iter().position(|a| a == "--effort").expect("--effort present");
        assert!(model_pos < dash_dash, "--model must precede --");
        assert!(effort_pos < dash_dash, "--effort must precede --");
        assert_eq!(args[model_pos + 1], "opus");
        assert_eq!(args[effort_pos + 1], "high");
    }

    /// No model/effort selected: no flags added, existing behavior unchanged.
    #[test]
    fn no_model_or_effort_omits_both_flags() {
        let args = build_args("hi", None, "/cfg.json", None, None);
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(!args.iter().any(|a| a == "--effort"));
    }
}
