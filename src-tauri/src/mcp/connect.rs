//! One-click platform connection. Instead of making the user hand-edit
//! `claude_desktop_config.json` / run `claude mcp add` / edit `config.toml`, dopedb
//! writes the config for them:
//!   - Claude Code : `claude mcp add --transport http … -s user`   (direct HTTP)
//!   - Codex CLI   : `codex mcp add dopedb -- <bridge>`           (stdio bridge)
//!   - Claude Desktop : merge `mcpServers.dopedb` into its JSON    (no CLI available)
//! Each user's own token is filled in from their local mcp.json, so no manual JSON.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    pub id: String,
    pub name: String,
    pub installed: bool,
    /// An `dopedb` entry already exists in this platform's MCP config.
    pub connected: bool,
    /// "http" (direct) | "bridge" (stdio) — how this platform reaches dopedb.
    pub method: String,
    pub note: String,
}

/// PATH used when spawning the platform CLIs. GUI apps launched from Finder inherit a
/// minimal PATH, and `claude` is a node shebang — so include the usual bin dirs.
fn augmented_path() -> String {
    let mut dirs: Vec<String> = vec![
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
    ];
    if let Some(home) = dirs::home_dir() {
        for sub in [".local/bin", ".bun/bin", ".npm-global/bin", ".volta/bin"] {
            dirs.push(home.join(sub).to_string_lossy().into_owned());
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        dirs.extend(path.split(':').map(String::from));
    }
    dirs.join(":")
}

fn which(bin: &str) -> Option<PathBuf> {
    augmented_path()
        .split(':')
        .map(|d| Path::new(d).join(bin))
        .find(|c| c.is_file())
}

fn claude_desktop_installed() -> bool {
    Path::new("/Applications/Claude.app").exists()
        || dirs::home_dir()
            .map(|h| h.join("Library/Application Support/Claude").is_dir())
            .unwrap_or(false)
}

/// `claude mcp get dopedb` exits 0 only when the server is registered. One CLI spawn
/// (~1s node startup) per detect — acceptable for a settings screen.
fn claude_code_connected(claude: Option<&PathBuf>) -> bool {
    claude.is_some_and(|bin| run(bin, &["mcp", "get", "dopedb"]).is_ok())
}

/// `codex mcp add` writes a `[mcp_servers.dopedb]` table; a text probe is enough.
fn codex_connected() -> bool {
    dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".codex/config.toml")).ok())
        .is_some_and(|s| s.contains("[mcp_servers.dopedb]"))
}

fn claude_desktop_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/Application Support/Claude/claude_desktop_config.json"))
}

fn claude_desktop_connected() -> bool {
    claude_desktop_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| v.get("mcpServers").and_then(|m| m.get("dopedb")).is_some())
}

pub fn detect() -> Vec<PlatformInfo> {
    let claude = which("claude");
    vec![
        PlatformInfo {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            installed: claude.is_some(),
            connected: claude_code_connected(claude.as_ref()),
            method: "http".into(),
            note: "Adds an HTTP server at user scope (run /mcp to see it).".into(),
        },
        PlatformInfo {
            id: "claude-desktop".into(),
            name: "Claude Desktop".into(),
            installed: claude_desktop_installed(),
            connected: claude_desktop_connected(),
            method: "bridge".into(),
            note: "Writes claude_desktop_config.json — restart Claude Desktop after.".into(),
        },
        PlatformInfo {
            id: "codex".into(),
            name: "Codex CLI".into(),
            installed: which("codex").is_some(),
            connected: codex_connected(),
            method: "bridge".into(),
            note: "Adds a stdio-bridge server to ~/.codex/config.toml.".into(),
        },
    ]
}

fn run(bin: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin)
        .args(args)
        .env("PATH", augmented_path())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { "command failed".into() } else { err })
    }
}

pub fn connect(id: &str, token: &str, url: &str, bridge_path: &str) -> Result<String, String> {
    match id {
        "claude-code" => {
            let bin = which("claude").ok_or("Claude Code (`claude`) not found")?;
            let _ = run(&bin, &["mcp", "remove", "dopedb", "-s", "user"]); // idempotent
            let header = format!("Authorization: Bearer {token}");
            run(
                &bin,
                &["mcp", "add", "--transport", "http", "dopedb", url, "-H", &header, "-s", "user"],
            )?;
            Ok("Connected to Claude Code. Run /mcp there to confirm.".into())
        }
        "codex" => {
            let bin = which("codex").ok_or("Codex (`codex`) not found")?;
            let _ = run(&bin, &["mcp", "remove", "dopedb"]); // idempotent
            run(&bin, &["mcp", "add", "dopedb", "--", bridge_path])?;
            Ok("Connected to Codex (stdio bridge).".into())
        }
        "claude-desktop" => connect_claude_desktop(bridge_path),
        other => Err(format!("unknown platform '{other}'")),
    }
}

/// Remove the `dopedb` MCP entry from a platform's config (inverse of [`connect`]).
/// Idempotent: removing an entry that is already gone succeeds.
pub fn disconnect(id: &str) -> Result<String, String> {
    match id {
        "claude-code" => {
            let bin = which("claude").ok_or("Claude Code (`claude`) not found")?;
            // No -s flag: removes from whichever scope holds it (we add at user scope,
            // but this also cleans up legacy local-scope entries).
            run(&bin, &["mcp", "remove", "dopedb"])?;
            Ok("Removed DopeDB from Claude Code.".into())
        }
        "codex" => {
            let bin = which("codex").ok_or("Codex (`codex`) not found")?;
            run(&bin, &["mcp", "remove", "dopedb"])?;
            Ok("Removed DopeDB from Codex.".into())
        }
        "claude-desktop" => disconnect_claude_desktop(),
        other => Err(format!("unknown platform '{other}'")),
    }
}

fn disconnect_claude_desktop() -> Result<String, String> {
    let path = claude_desktop_config_path().ok_or("no home dir")?;
    if !path.exists() {
        return Ok("Already removed (no Claude Desktop config).".into());
    }
    let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    // Same rule as connect: never rewrite a config we couldn't parse.
    let mut root: serde_json::Value = serde_json::from_str(&s).map_err(|e| {
        format!("existing claude_desktop_config.json is not valid JSON ({e}); fix it manually")
    })?;
    let removed = root
        .get_mut("mcpServers")
        .and_then(|m| m.as_object_mut())
        .and_then(|m| m.remove("dopedb"))
        .is_some();
    if !removed {
        return Ok("Already removed.".into());
    }
    let _ = std::fs::write(path.with_extension("json.dopedb-bak"), &s);
    let pretty = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, pretty).map_err(|e| e.to_string())?;
    Ok("Removed — restart Claude Desktop to apply.".into())
}

fn connect_claude_desktop(bridge_path: &str) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let path = home.join("Library/Application Support/Claude/claude_desktop_config.json");

    let mut root: serde_json::Value = if path.exists() {
        let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        // ABORT on unparseable JSON — never silently replace the user's config with {}.
        let parsed: serde_json::Value = serde_json::from_str(&s).map_err(|e| {
            format!(
                "existing claude_desktop_config.json is not valid JSON ({e}); \
                 fix or remove it and retry — refusing to overwrite it"
            )
        })?;
        // Back up only a file that parsed OK, so a known-good backup is never clobbered
        // by a broken current state.
        let _ = std::fs::write(path.with_extension("json.dopedb-bak"), &s);
        parsed
    } else {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        serde_json::json!({})
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let obj = root.as_object_mut().unwrap();
    let servers = obj.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    servers
        .as_object_mut()
        .unwrap()
        .insert("dopedb".into(), serde_json::json!({ "command": bridge_path }));

    let pretty = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, pretty).map_err(|e| e.to_string())?;
    Ok("Wrote claude_desktop_config.json — restart Claude Desktop to load it.".into())
}
