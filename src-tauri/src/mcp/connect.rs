//! One-click platform connection. Instead of making the user hand-edit
//! `claude_desktop_config.json` / run `claude mcp add` / edit `config.toml`, dopedb
//! writes the config for them:
//!   - Claude Code : `claude mcp add --transport http … -s user`   (direct HTTP)
//!   - Codex CLI   : `codex mcp add dopedb -- <bridge>`           (stdio bridge)
//!   - Codex Desktop : merge `[mcp_servers.dopedb]` into config.toml
//!   - Claude Desktop : merge `mcpServers.dopedb` into its JSON    (no CLI available)
//!
//! Each user's own token is filled in from their local mcp.json, so no manual JSON.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Serialize;
use toml_edit::{value, DocumentMut, Item, Table};

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
fn augmented_path() -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();

    #[cfg(not(windows))]
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
            .into_iter()
            .map(PathBuf::from),
    );

    if let Some(home) = dirs::home_dir() {
        #[cfg(not(windows))]
        for sub in [".local/bin", ".bun/bin", ".npm-global/bin", ".volta/bin"] {
            dirs.push(home.join(sub));
        }

        #[cfg(windows)]
        {
            dirs.push(home.join(".local/bin"));
            dirs.push(home.join("AppData/Local/Programs/OpenAI/Codex/bin"));
            dirs.push(home.join("AppData/Local/Microsoft/WindowsApps"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }

    std::env::join_paths(dirs).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

fn bin_names(bin: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        let path = Path::new(bin);
        if path.extension().is_some() {
            return vec![bin.to_string()];
        }
        ["exe", "cmd", "bat"]
            .into_iter()
            .map(|ext| format!("{bin}.{ext}"))
            .chain(std::iter::once(bin.to_string()))
            .collect()
    }

    #[cfg(not(windows))]
    {
        vec![bin.to_string()]
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let names = bin_names(bin);
    std::env::split_paths(&augmented_path())
        .flat_map(|d| names.iter().map(move |name| d.join(name)))
        .find(|c| c.is_file())
}

fn claude_desktop_installed() -> bool {
    claude_desktop_config_path()
        .and_then(|p| p.parent().map(Path::is_dir))
        .unwrap_or(false)
        || {
            #[cfg(target_os = "macos")]
            {
                Path::new("/Applications/Claude.app").exists()
            }
            #[cfg(not(target_os = "macos"))]
            {
                false
            }
        }
}

/// `claude mcp get dopedb` exits 0 only when the server is registered. One CLI spawn
/// (~1s node startup) per detect — acceptable for a settings screen.
fn claude_code_connected(claude: Option<&PathBuf>) -> bool {
    claude.is_some_and(|bin| run(bin, &["mcp", "get", "dopedb"]).is_ok())
}

/// `codex mcp add` writes a `[mcp_servers.dopedb]` table; a text probe is enough.
fn codex_config_dir() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
}

fn codex_config_path() -> Option<PathBuf> {
    codex_config_dir().map(|d| d.join("config.toml"))
}

fn codex_config_connected() -> bool {
    codex_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|s| s.contains("[mcp_servers.dopedb]"))
}

fn claude_desktop_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .map(|h| h.join("Library/Application Support/Claude/claude_desktop_config.json"))
    }

    #[cfg(windows)]
    {
        dirs::config_dir().map(|d| d.join("Claude/claude_desktop_config.json"))
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        dirs::config_dir().map(|d| d.join("Claude/claude_desktop_config.json"))
    }
}

fn claude_desktop_connected() -> bool {
    claude_desktop_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| v.get("mcpServers").and_then(|m| m.get("dopedb")).is_some())
}

fn codex_desktop_installed() -> bool {
    codex_config_dir().is_some_and(|d| d.is_dir()) || {
        #[cfg(target_os = "macos")]
        {
            Path::new("/Applications/Codex.app").exists()
                || dirs::home_dir()
                    .map(|h| h.join("Applications/Codex.app").exists())
                    .unwrap_or(false)
        }
        #[cfg(windows)]
        {
            dirs::data_local_dir()
                .map(|d| d.join("Programs/OpenAI/Codex").is_dir())
                .unwrap_or(false)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            false
        }
    }
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
            connected: codex_config_connected(),
            method: "bridge".into(),
            note: "Adds a stdio-bridge server to ~/.codex/config.toml.".into(),
        },
        PlatformInfo {
            id: "codex-desktop".into(),
            name: "Codex Desktop".into(),
            installed: codex_desktop_installed(),
            connected: codex_config_connected(),
            method: "bridge".into(),
            note: "Writes ~/.codex/config.toml directly; restart Codex Desktop after.".into(),
        },
    ]
}

/// A `Command` that never pops up a console window on Windows. Release builds run
/// with `windows_subsystem = "windows"` (no attached console), so plainly spawning a
/// console child (`claude.cmd`, `cmd`) makes Windows open a visible one — the
/// Settings screen would flash a black window on every platform detect.
fn quiet_command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run(bin: &Path, args: &[&str]) -> Result<String, String> {
    let out = quiet_command(bin)
        .args(args)
        .env("PATH", augmented_path())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            "command failed".into()
        } else {
            err
        })
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
                &[
                    "mcp",
                    "add",
                    "--transport",
                    "http",
                    "dopedb",
                    url,
                    "-H",
                    &header,
                    "-s",
                    "user",
                ],
            )?;
            Ok("Connected to Claude Code. Run /mcp there to confirm.".into())
        }
        "codex" => {
            let bin = which("codex").ok_or("Codex (`codex`) not found")?;
            let _ = run(&bin, &["mcp", "remove", "dopedb"]); // idempotent
            run(&bin, &["mcp", "add", "dopedb", "--", bridge_path])?;
            Ok("Connected to Codex (stdio bridge).".into())
        }
        "codex-desktop" => connect_codex_config(bridge_path),
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
        "codex-desktop" => disconnect_codex_config(),
        "claude-desktop" => disconnect_claude_desktop(),
        other => Err(format!("unknown platform '{other}'")),
    }
}

pub fn open_app(id: &str) -> Result<String, String> {
    let app = match id {
        "codex" | "codex-desktop" => "Codex",
        "claude-code" | "claude-desktop" => "Claude",
        other => return Err(format!("unknown platform '{other}'")),
    };
    open_named_app(app)
}

#[cfg(target_os = "macos")]
fn open_named_app(name: &str) -> Result<String, String> {
    let status = Command::new("open")
        .args(["-a", name])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(format!("Opened {name}."))
    } else {
        Err(format!("{name} is not installed or could not be opened"))
    }
}

#[cfg(windows)]
fn open_named_app(name: &str) -> Result<String, String> {
    // `start` detaches the target app, so CREATE_NO_WINDOW only hides the
    // intermediate cmd.exe console — the launched app opens normally.
    let status = quiet_command("cmd")
        .args(["/C", "start", "", name])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(format!("Opened {name}."))
    } else {
        Err(format!("{name} is not installed or could not be opened"))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_named_app(name: &str) -> Result<String, String> {
    Err(format!("Opening {name} from DopeDB is not supported on this OS yet"))
}

fn load_codex_config() -> Result<(PathBuf, String, DocumentMut), String> {
    let path = codex_config_path().ok_or("no home dir")?;
    let raw = if path.exists() {
        std::fs::read_to_string(&path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };
    let doc = raw
        .parse::<DocumentMut>()
        .map_err(|e| format!("existing config.toml is not valid TOML ({e}); fix it manually"))?;
    Ok((path, raw, doc))
}

fn codex_servers_table(doc: &mut DocumentMut) -> Result<&mut Table, String> {
    if !doc.as_table().contains_key("mcp_servers") {
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| "config.toml has non-table mcp_servers; fix it manually".into())
}

fn connect_codex_config(bridge_path: &str) -> Result<String, String> {
    let (path, raw, mut doc) = load_codex_config()?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    if path.exists() {
        let _ = std::fs::write(path.with_extension("toml.dopedb-bak"), &raw);
    }

    let mut server = Table::new();
    server["command"] = value(bridge_path);
    codex_servers_table(&mut doc)?.insert("dopedb", Item::Table(server));

    std::fs::write(&path, doc.to_string()).map_err(|e| e.to_string())?;
    Ok("Wrote ~/.codex/config.toml — restart Codex Desktop to load it.".into())
}

fn disconnect_codex_config() -> Result<String, String> {
    let (path, raw, mut doc) = load_codex_config()?;
    if !path.exists() {
        return Ok("Already removed (no Codex config).".into());
    }
    let removed = doc["mcp_servers"]
        .as_table_mut()
        .and_then(|servers| servers.remove("dopedb"))
        .is_some();
    if !removed {
        return Ok("Already removed.".into());
    }
    let _ = std::fs::write(path.with_extension("toml.dopedb-bak"), &raw);
    std::fs::write(&path, doc.to_string()).map_err(|e| e.to_string())?;
    Ok("Removed DopeDB from Codex config — restart Codex Desktop to apply.".into())
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
    let path = claude_desktop_config_path().ok_or("no home dir")?;

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
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    servers.as_object_mut().unwrap().insert(
        "dopedb".into(),
        serde_json::json!({ "command": bridge_path }),
    );

    let pretty = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, pretty).map_err(|e| e.to_string())?;
    Ok("Wrote claude_desktop_config.json — restart Claude Desktop to load it.".into())
}
