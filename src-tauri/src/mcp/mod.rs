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

use crate::connection::Live;
use crate::state::McpRuntime;
use crate::store::Store;
use tools::{DbTools, QueryPlanStore};

pub(crate) use tools::query_plan_store;

/// Shared live-connection cache — the SAME instance as `AppState.connections`, so a
/// connection edit/delete evicts the agent's cached pool too.
pub type SharedConns = Arc<Mutex<HashMap<Uuid, Live>>>;

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
    mcp_url_for_port(MCP_PORT)
}

fn mcp_url_for_port(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
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
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) {
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
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resolve the process token's user SID without relying on a localized or ambiguous
/// account name such as `%USERNAME%`.
#[cfg(windows)]
fn current_user_sid() -> std::io::Result<String> {
    use std::io::{Error, ErrorKind};
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER,
    };
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = null_mut();
    // SAFETY: `token` is a valid out-pointer, and the pseudo process handle is valid
    // for the lifetime of this call. A successful real token handle is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(Error::last_os_error());
    }

    let result = (|| {
        let mut byte_len = 0u32;
        // SAFETY: the null/zero probe is the documented way to obtain the required
        // TOKEN_USER buffer length; no memory is read through the null pointer.
        let probe = unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut byte_len) };
        // SAFETY: GetLastError is read immediately after the failed Win32 call.
        let probe_error = unsafe { GetLastError() };
        if probe != 0 || probe_error != ERROR_INSUFFICIENT_BUFFER || byte_len == 0 {
            return Err(Error::from_raw_os_error(probe_error as i32));
        }

        // usize storage supplies enough alignment for TOKEN_USER while byte_len remains
        // the exact size passed back to Windows.
        let words = (byte_len as usize).div_ceil(std::mem::size_of::<usize>());
        let mut buffer = vec![0usize; words];
        // SAFETY: the buffer is writable, aligned, and at least byte_len bytes long.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                byte_len,
                &mut byte_len,
            )
        } == 0
        {
            return Err(Error::last_os_error());
        }

        // SAFETY: a successful TokenUser query initialized the buffer as TOKEN_USER,
        // whose SID pointer remains valid while `buffer` is alive.
        let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_text = null_mut();
        // SAFETY: the SID came from the current process token and sid_text is a valid
        // out-pointer. Windows allocates the returned null-terminated UTF-16 string.
        if unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid_text) } == 0 {
            return Err(Error::last_os_error());
        }

        let mut len = 0usize;
        // SAFETY: ConvertSidToStringSidW guarantees a null-terminated UTF-16 string.
        while unsafe { *sid_text.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` was found within the allocated null-terminated string.
        let units = unsafe { std::slice::from_raw_parts(sid_text, len) };
        let sid = String::from_utf16(units).map_err(|e| Error::new(ErrorKind::InvalidData, e));
        // SAFETY: this pointer was allocated by ConvertSidToStringSidW with LocalAlloc.
        unsafe { LocalFree(sid_text.cast()) };
        sid
    })();

    // SAFETY: OpenProcessToken returned this owned handle successfully above.
    unsafe { CloseHandle(token) };
    result
}

#[cfg(windows)]
fn run_hidden_icacls<I, S>(path: &std::path::Path, args: I) -> std::io::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    use std::os::windows::process::CommandExt;

    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let detail = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        Err(std::io::Error::other(format!(
            "icacls exited with {}: {}",
            output.status,
            String::from_utf8_lossy(detail).trim()
        )))
    }
}

#[cfg(windows)]
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) {
    let sid = match current_user_sid() {
        Ok(sid) => sid,
        Err(e) => {
            tracing::error!(
                "cannot resolve current SID; refusing to write {}: {e}",
                path.display()
            );
            return;
        }
    };

    if let Err(e) = std::fs::write(path, bytes) {
        tracing::error!("failed to write {}: {e}", path.display());
        return;
    }

    // `/reset` replaces every explicit ACE with inherited defaults. Removing those
    // inherited ACEs next leaves an empty protected DACL, then the numeric current-user
    // SID becomes the sole full-control principal. SYSTEM and Administrators are not
    // retained: this file contains a bearer token and follows the Unix 0600 policy.
    // Every child process is hidden so a GUI startup never flashes a console window.
    let grant = format!("*{sid}:F");
    let restricted = run_hidden_icacls(path, ["/reset", "/q"])
        .and_then(|_| run_hidden_icacls(path, ["/inheritance:r", "/q"]))
        .and_then(|_| run_hidden_icacls(path, ["/grant:r", grant.as_str(), "/q"]));
    if let Err(e) = restricted {
        tracing::error!(
            "failed to replace ACL for {}; deleting the token file: {e}",
            path.display()
        );
        if let Err(remove_error) = std::fs::remove_file(path) {
            tracing::error!(
                "failed to remove insecure token file {}: {remove_error}",
                path.display()
            );
        }
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

    // The installed app owns the stable external-integration port. Development often
    // runs alongside it, so an address collision gets a process-local fallback used by
    // in-app Agent chat rather than routing the child CLI into the other app instance.
    let (listener, bind_warning) =
        match tokio::net::TcpListener::bind(("127.0.0.1", MCP_PORT)).await {
            Ok(listener) => (listener, None),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
                (
                    listener,
                    Some(format!(
                        "MCP HTTP port {MCP_PORT} is used by another DopeDB instance; \
                         in-app Agent chat is using a temporary local port."
                    )),
                )
            }
            Err(e) => {
                let mut rt = runtime.lock().unwrap();
                rt.http_running = false;
                rt.http_port = None;
                rt.http_url = None;
                rt.http_fallback = false;
                rt.last_error = Some(format!("MCP HTTP bind on 127.0.0.1:{MCP_PORT} failed: {e}"));
                return Err(e);
            }
        };
    let actual_port = match listener.local_addr() {
        Ok(address) => address.port(),
        Err(e) => {
            let mut rt = runtime.lock().unwrap();
            rt.http_running = false;
            rt.http_port = None;
            rt.http_url = None;
            rt.http_fallback = false;
            rt.last_error = Some(format!("MCP HTTP listener address unavailable: {e}"));
            return Err(e);
        }
    };
    let actual_url = mcp_url_for_port(actual_port);
    {
        let mut rt = runtime.lock().unwrap();
        rt.http_running = true;
        rt.http_port = Some(actual_port);
        rt.http_url = Some(actual_url.clone());
        rt.http_fallback = actual_port != MCP_PORT;
        rt.last_error = bind_warning.clone();
    }
    if let Some(warning) = bind_warning {
        tracing::warn!("{warning}");
    }
    tracing::info!("MCP server listening on {actual_url}");
    let r = axum::serve(listener, router).await;
    {
        let mut rt = runtime.lock().unwrap();
        rt.http_running = false;
        rt.http_port = None;
        rt.http_url = None;
        rt.http_fallback = false;
    }
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
        "httpPort": rt.http_port,
        "httpUrl": rt.http_url,
        "httpFallback": rt.http_fallback,
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

        std::fs::write(&path, b"seed").unwrap();
        // Seed an explicit Everyone ACE. /inheritance:r + /grant:r alone would leave
        // this behind, which is the regression covered by issue #20.
        run_hidden_icacls(&path, ["/grant", "*S-1-1-0:R", "/q"]).unwrap();

        write_private(&path, b"first");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");

        let listing = run_hidden_icacls(&path, std::iter::empty::<&str>()).unwrap();
        assert!(
            !listing.contains("(I)"),
            "expected no inherited ACEs, got: {listing}"
        );
        assert_eq!(
            listing.matches(":(").count(),
            1,
            "expected only the current-user allow ACE, got: {listing}"
        );
        assert!(
            listing.contains("(F)"),
            "expected full control, got: {listing}"
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
        assert!(
            bridge_binary_candidates(exe, "bridge").contains(&PathBuf::from(
                "/Applications/DopeDB.app/Contents/Resources/bridge"
            ))
        );
    }

    #[cfg(windows)]
    #[test]
    fn bridge_binary_path_targets_the_exe_sidecar() {
        // Generated MCP configs embed this path; without the .exe suffix Windows
        // platforms cannot spawn the bridge.
        assert!(bridge_binary_path().ends_with("dopedb-mcp-stdio.exe"));
    }
}
