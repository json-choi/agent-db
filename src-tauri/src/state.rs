//! Shared application state managed by Tauri and injected into commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::connection::Live;
use crate::error::AppResult;
use crate::store::Store;

/// Live runtime status of the MCP listeners, set by `mcp::serve_*` on bind
/// success/failure so the UI can tell "actually listening" from "config exists".
#[derive(Default)]
pub struct McpRuntime {
    pub http_running: bool,
    pub bridge_running: bool,
    /// Actual HTTP listener chosen for this process. A second development instance
    /// falls back to an ephemeral port instead of silently talking to another app.
    pub http_port: Option<u16>,
    pub http_url: Option<String>,
    pub http_fallback: bool,
    pub last_error: Option<String>,
}

pub struct AppState {
    /// Handle to the local app.db (connections, safety, history, audit, schema cache).
    pub store: Store,
    /// Open, live DB connections keyed by connection id.
    // ponytail: one global mutex over the whole map; fine for a single-user desktop
    // app. Move to a per-connection lock only if concurrent queries contend.
    // Arc so the MCP listeners share THIS instance (evictions from upsert/delete
    // reach the MCP server's cached pools too — not a separate map).
    pub connections: Arc<Mutex<HashMap<Uuid, Live>>>,
    /// Bearer token guarding the local MCP server (persisted in mcp.json).
    pub mcp_token: String,
    /// Live status of the MCP HTTP + bridge listeners.
    pub mcp_runtime: Arc<Mutex<McpRuntime>>,
    /// In-app agent chat memory (resumable CLI session id + active-turn tracking).
    pub chat: crate::agent::ChatState,
    /// Safety-sensitive rollout gates captured once for this app runtime.
    pub features: crate::features::FeatureFlags,
}

impl AppState {
    pub async fn new() -> AppResult<Self> {
        Ok(Self {
            store: Store::open().await?,
            connections: Arc::new(Mutex::new(HashMap::new())),
            mcp_token: crate::mcp::load_or_create_token(),
            mcp_runtime: Arc::new(Mutex::new(McpRuntime::default())),
            chat: crate::agent::chat_state(),
            features: crate::features::FeatureFlags::default(),
        })
    }
}
