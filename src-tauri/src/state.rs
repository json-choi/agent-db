//! Shared application state managed by Tauri and injected into commands.

use std::sync::{Arc, Mutex};

use crate::connection::ConnectionManager;
use crate::error::AppResult;
use crate::features::{FeatureFlag, FeatureFlags};
use crate::operations::{LocalApprovalAuthority, OperationRuntime};
use crate::services::ApplicationServices;
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
    /// Scope-pinned, per-connection single-flight pool owner shared with every adapter.
    pub connections: ConnectionManager,
    /// Transport-neutral application services shared by Tauri and future adapters.
    pub services: ApplicationServices,
    /// Bearer token guarding the local MCP server (persisted in mcp.json).
    pub mcp_token: String,
    /// Live status of the MCP HTTP + bridge listeners.
    pub mcp_runtime: Arc<Mutex<McpRuntime>>,
    /// In-app agent chat memory (resumable CLI session id + active-turn tracking).
    pub chat: crate::agent::ChatState,
    /// Safety-sensitive rollout gates captured once for this app runtime.
    pub features: crate::features::FeatureFlags,
    /// Desktop-only approval capability. MCP/CLI/Agent adapters receive only the
    /// ApplicationServices facade and therefore cannot obtain this value.
    pub(crate) local_operation_approval: LocalApprovalAuthority,
}

impl AppState {
    pub async fn new() -> AppResult<Self> {
        let features = FeatureFlags::new([FeatureFlag::OperationRuntimeV1]);
        let store = Store::open().await?;
        let connections = ConnectionManager::new(store.clone());
        let (operation, local_operation_approval) = OperationRuntime::new(&store);
        let services = ApplicationServices::new(store.clone(), connections.clone(), operation);
        if features.is_enabled(FeatureFlag::OperationRuntimeV1) {
            services.operation.recover_previous_runtimes().await?;
        }
        Ok(Self {
            store,
            connections,
            services,
            mcp_token: crate::mcp::load_or_create_token(),
            mcp_runtime: Arc::new(Mutex::new(McpRuntime::default())),
            chat: crate::agent::chat_state(),
            features,
            local_operation_approval,
        })
    }
}
