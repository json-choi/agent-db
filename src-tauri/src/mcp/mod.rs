//! MCP server (Phase 0 spike): the app hosts a Streamable HTTP MCP server on
//! `127.0.0.1:7686/mcp`. Subscription platforms (Claude Code, Cursor, …) dial it;
//! Claude Desktop reaches it via a stdio bridge (Phase 3). The Rust core owns the
//! connections, credentials, and safety pipeline; the MCP tools are a thin surface
//! over the existing modules (see `tools.rs`).
//!
//! Security (defense-in-depth around L2/L4): bound to loopback only, `rmcp`'s Host
//! validation (DNS-rebind protection) is ON by default (allowed_hosts =
//! localhost/127.0.0.1/::1), and a bearer token is required on every request.

pub mod connect;
pub mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use tauri::AppHandle;
use uuid::Uuid;

use crate::connection::LiveConnection;
use crate::state::McpRuntime;
use crate::store::Store;
use tools::{DbTools, QueryPlanStore};

pub(crate) use tools::query_plan_store;

/// Shared live-connection cache — the SAME instance as `AppState.connections`, so a
/// connection edit/delete evicts the agent's cached pool too.
pub type SharedConns = Arc<Mutex<HashMap<Uuid, LiveConnection>>>;

/// Fixed loopback port for the Streamable HTTP MCP endpoint.
pub const MCP_PORT: u16 = 7686;
/// Fixed loopback port for the raw line-framed MCP listener the stdio bridge dials
/// (for Claude Desktop, which can't reach a localhost HTTP server).
pub const MCP_BRIDGE_PORT: u16 = 7687;

/// Absolute path to the bundled stdio-bridge binary. Used in generated MCP configs;
/// GUI-spawned children get a minimal PATH, so the config must reference it directly.
pub fn bridge_binary_path() -> String {
    let name = if cfg!(windows) {
        "dopedb-mcp-stdio.exe"
    } else {
        "dopedb-mcp-stdio"
    };

    if let Ok(exe) = std::env::current_exe() {
        let candidates = bridge_binary_candidates(&exe, name);
        if let Some(path) = candidates.iter().find(|path| path.is_file()) {
            return path.to_string_lossy().into_owned();
        }

        // Not built yet: still return the primary installed/dev location so the config
        // points where the sidecar should land, not a bare name a GUI-spawned child
        // cannot resolve through PATH.
        if let Some(path) = candidates.into_iter().next() {
            return path.to_string_lossy().into_owned();
        }
    }

    name.into()
}

fn bridge_binary_candidates(exe: &Path, name: &str) -> Vec<PathBuf> {
    let Some(exe_dir) = exe.parent() else {
        return Vec::new();
    };

    let mut candidates = vec![
        // Tauri externalBin is copied next to the main binary on macOS
        // (Contents/MacOS) and Windows install layouts.
        exe_dir.join(name),
        // Keep a resource-dir fallback for updater/bundler layout changes.
        exe_dir.join("resources").join(name),
    ];

    // macOS app bundle fallback: /Applications/DopeDB.app/Contents/Resources.
    if exe_dir.file_name().is_some_and(|part| part == "MacOS") {
        if let Some(contents_dir) = exe_dir.parent() {
            candidates.push(contents_dir.join("Resources").join(name));
        }
    }

    candidates.dedup();
    candidates
}

pub fn mcp_json_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_default()
        .join("dopedb")
        .join("mcp.json")
}

pub fn mcp_url() -> String {
    format!("http://127.0.0.1:{MCP_PORT}/mcp")
}

/// Load the persisted bearer token, or mint + persist a new 256-bit one. Persisted so
/// a config the user pasted into their platform keeps working across restarts.
pub fn load_or_create_token() -> String {
    let path = mcp_json_path();
    // Reuse an existing token so pasted configs keep working; else mint a 256-bit one
    // (two v4 UUIDs, no extra dependency).
    let existing = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(String::from))
        .filter(|t| !t.is_empty());
    let token = existing
        .unwrap_or_else(|| format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()));

    // Always refresh mcp.json so the bridge port + binary path are current for the
    // bridge to read and for the UI's config snippets. It holds the bearer token, so
    // write it 0600 and surface failures — a silently unwritten token breaks connect.
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = serde_json::json!({
        "port": MCP_PORT,
        "bridgePort": MCP_BRIDGE_PORT,
        "token": token,
        "url": mcp_url(),
        "bridgePath": bridge_binary_path(),
    })
    .to_string();
    write_private(&path, body.as_bytes());
    token
}

/// Write `bytes` to `path` privately — the file holds the MCP bearer token. On Unix
/// we force 0600; on Windows we strip ACL inheritance and grant only the current
/// user (the same shape OpenSSH requires for private keys on Windows).
#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(bytes) {
                tracing::error!("failed to write {}: {e}", path.display());
            }
            // Enforce 0600 even if the file pre-existed with looser perms (mode() only
            // applies on create).
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Err(e) => tracing::error!("failed to open {} for write: {e}", path.display()),
    }
}

#[cfg(windows)]
fn write_private(path: &std::path::Path, bytes: &[u8]) {
    use std::os::windows::process::CommandExt;

    if let Err(e) = std::fs::write(path, bytes) {
        tracing::error!("failed to write {}: {e}", path.display());
        return;
    }

    // Mirror the Unix 0600 branch: drop inherited ACEs and grant only the current
    // user. %APPDATA% usually inherits user-only access already, but a relocated
    // profile or loosened parent ACL would otherwise expose the bearer token.
    // Failure is non-fatal — we fall back to the inherited ACLs and log it.
    let user = match std::env::var("USERNAME") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            tracing::warn!(
                "USERNAME unset; leaving {} on inherited ACLs",
                path.display()
            );
            return;
        }
    };
    // The app runs without a console (windows_subsystem = "windows"); without
    // CREATE_NO_WINDOW this spawn would flash a console at every startup.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let result = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match result {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            "icacls could not restrict {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => tracing::warn!("could not run icacls for {}: {e}", path.display()),
    }
}

/// Constant-time token check via SHA-256 digests: never early-exits on the plaintext
/// token, so a byte-by-byte timing oracle can't recover it.
fn token_matches(candidate: &str, token: &str) -> bool {
    use sha2::{Digest, Sha256};
    let a = Sha256::digest(candidate.as_bytes());
    let b = Sha256::digest(token.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Bearer-token gate. `rmcp` already validates Host (DNS-rebind); this adds auth so a
/// local process/browser can't call the DB tools without the token.
async fn require_bearer(
    State(token): State<String>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| token_matches(t, &token))
        .unwrap_or(false);
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Start the Streamable HTTP MCP server on `127.0.0.1:MCP_PORT`. Runs for the process
/// lifetime (kill switch / graceful shutdown is Phase 3).
pub async fn serve_mcp(
    app: AppHandle,
    store: Store,
    token: String,
    conns: SharedConns,
    plans: QueryPlanStore,
    runtime: Arc<Mutex<McpRuntime>>,
) -> std::io::Result<()> {
    let service = StreamableHttpService::new(
        move || {
            Ok::<_, std::io::Error>(DbTools::new(
                store.clone(),
                app.clone(),
                conns.clone(),
                plans.clone(),
            ))
        },
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );

    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(token, require_bearer));

    // Bind first so we can report a taken port instead of the UI lying "Running".
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", MCP_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            let mut rt = runtime.lock().unwrap();
            rt.http_running = false;
            rt.last_error = Some(format!("MCP HTTP bind on 127.0.0.1:{MCP_PORT} failed: {e}"));
            return Err(e);
        }
    };
    runtime.lock().unwrap().http_running = true;
    tracing::info!("MCP server listening on {}", mcp_url());
    let r = axum::serve(listener, router).await;
    runtime.lock().unwrap().http_running = false;
    r
}

/// Raw line-framed MCP listener that the stdio bridge dials. For each connection: read
/// one line (the bearer token), verify it, then serve MCP over the TCP stream — the same
/// `DbTools` handler, so Claude Desktop gets identical tools + safety over stdio.
pub async fn serve_stdio_bridge(
    app: AppHandle,
    store: Store,
    token: String,
    conns: SharedConns,
    plans: QueryPlanStore,
    runtime: Arc<Mutex<McpRuntime>>,
) -> std::io::Result<()> {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", MCP_BRIDGE_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            let mut rt = runtime.lock().unwrap();
            rt.bridge_running = false;
            rt.last_error = Some(format!(
                "MCP bridge bind on 127.0.0.1:{MCP_BRIDGE_PORT} failed: {e}"
            ));
            return Err(e);
        }
    };
    runtime.lock().unwrap().bridge_running = true;
    tracing::info!("MCP stdio-bridge listener on 127.0.0.1:{MCP_BRIDGE_PORT}");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("bridge accept error: {e}");
                continue;
            }
        };
        let store = store.clone();
        let app = app.clone();
        let conns = conns.clone();
        let plans = plans.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let (r, w) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(r);
            // Bounded auth read: any local process could otherwise stream a newline-free
            // flood and balloon memory. The token line is short (64 hex chars).
            let first = match read_auth_line(&mut reader, 4096).await {
                Some(line) => line,
                None => return,
            };
            if !token_matches(first.trim(), &token) {
                tracing::warn!("bridge auth failed — dropping connection");
                return; // unauthenticated — drop the connection
            }
            let handler = DbTools::new(store, app, conns, plans);
            match rmcp::serve_server(handler, (reader, w)).await {
                Ok(service) => {
                    let _ = service.waiting().await;
                }
                Err(e) => tracing::warn!("bridge MCP serve error: {e}"),
            }
        });
    }
}

/// Read one `\n`-terminated line, capped at `max` bytes. Returns None on EOF/IO error or
/// if the cap is hit before a newline (a newline-free flood). Leaves the rest buffered in
/// `reader` for the MCP stream that follows.
async fn read_auth_line<R>(reader: &mut R, max: usize) -> Option<String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).await {
            Ok(0) => return None, // EOF before newline
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Some(String::from_utf8_lossy(&buf).into_owned());
                }
                if buf.len() >= max {
                    tracing::warn!("bridge auth line exceeded {max} bytes — dropping connection");
                    return None;
                }
                buf.push(byte[0]);
            }
            Err(_) => return None,
        }
    }
}

/// Live MCP listener status for the UI — distinguishes "actually listening" from the
/// static config `mcp_status` returns. `error` is the last bind failure, if any.
#[tauri::command]
pub fn mcp_runtime_status(state: tauri::State<'_, crate::state::AppState>) -> serde_json::Value {
    let rt = state.mcp_runtime.lock().unwrap();
    serde_json::json!({
        "httpRunning": rt.http_running,
        "bridgeRunning": rt.bridge_running,
        "error": rt.last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_ct() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abcd")); // prefix, differing length
        assert!(!token_matches("abd", "abc")); // same length, last byte differs
        assert!(!token_matches("", "abc"));
    }

    /// The ACL restriction must neither corrupt the payload nor break the
    /// rewrite-on-every-startup behavior of `load_or_create_token`.
    #[cfg(windows)]
    #[test]
    fn write_private_restricts_acl_and_stays_rewritable() {
        let path =
            std::env::temp_dir().join(format!("dopedb-write-private-{}.json", Uuid::new_v4()));

        write_private(&path, b"first");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");

        // Inheritance stripped: icacls marks inherited ACEs with "(I)".
        let listing = std::process::Command::new("icacls")
            .arg(&path)
            .output()
            .expect("icacls runs");
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        assert!(
            !listing.contains("(I)"),
            "expected no inherited ACEs, got: {listing}"
        );

        write_private(&path, b"second");
        assert_eq!(std::fs::read(&path).unwrap(), b"second");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bridge_candidates_probe_exe_dir_then_resources() {
        let exe = Path::new("/opt/dopedb/dopedb");
        assert_eq!(
            bridge_binary_candidates(exe, "bridge"),
            [
                PathBuf::from("/opt/dopedb/bridge"),
                PathBuf::from("/opt/dopedb/resources/bridge"),
            ]
        );
    }

    #[test]
    fn bridge_candidates_include_macos_bundle_resources() {
        let exe = Path::new("/Applications/DopeDB.app/Contents/MacOS/dopedb");
        assert!(bridge_binary_candidates(exe, "bridge")
            .contains(&PathBuf::from("/Applications/DopeDB.app/Contents/Resources/bridge")));
    }

    #[cfg(windows)]
    #[test]
    fn bridge_binary_path_targets_the_exe_sidecar() {
        // Generated MCP configs embed this path; without the .exe suffix Windows
        // platforms cannot spawn the bridge.
        assert!(bridge_binary_path().ends_with("dopedb-mcp-stdio.exe"));
    }
}
