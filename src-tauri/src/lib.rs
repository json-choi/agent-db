//! agent-db — Rust core entrypoint. Wires modules, state, and the Tauri command
//! surface. Agents drive the DB through the local MCP server (see the `mcp` module).

mod audit;
mod commands;
mod connection;
mod error;
mod executor;
mod introspect;
mod mcp;
mod migrations;
mod model;
mod safety;
mod state;
mod store;

pub use error::{AppError, AppResult};

use tauri::Manager;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let state = tauri::async_runtime::block_on(state::AppState::new())
        .expect("failed to initialize app state");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(|app| {
            // Start the local MCP server (Streamable HTTP on 127.0.0.1:7686). It shares
            // the app's Store + Keychain + safety pipeline via the tools in `mcp`.
            let st = app.state::<state::AppState>();
            let store = st.store.clone();
            let token = st.mcp_token.clone();
            // Share the SAME live-connection map + runtime-status cell as AppState, so
            // connection edits/deletes evict the agent's pools and the UI can tell
            // "actually listening" from "config exists".
            let conns = st.connections.clone();
            let runtime = st.mcp_runtime.clone();
            let handle = app.handle().clone();
            // HTTP endpoint (Claude Code / Cursor / …).
            {
                let (store, token, handle, conns, runtime) = (
                    store.clone(),
                    token.clone(),
                    handle.clone(),
                    conns.clone(),
                    runtime.clone(),
                );
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = mcp::serve_mcp(handle, store, token, conns, runtime).await {
                        tracing::error!("MCP HTTP server failed: {e}");
                    }
                });
            }
            // Raw TCP listener the stdio bridge dials (Claude Desktop).
            tauri::async_runtime::spawn(async move {
                if let Err(e) = mcp::serve_stdio_bridge(handle, store, token, conns, runtime).await {
                    tracing::error!("MCP stdio-bridge failed: {e}");
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_connections,
            commands::upsert_connection,
            commands::delete_connection,
            commands::test_connection,
            commands::test_connection_profile,
            commands::get_schema,
            commands::refresh_schema,
            introspect::get_table_ddl,
            commands::classify_sql,
            commands::preview_sql,
            commands::run_sql,
            commands::run_script,
            commands::get_safety,
            commands::set_safety,
            commands::list_audit,
            commands::audit_verify,
            commands::list_history,
            commands::mcp_status,
            commands::mcp_platforms,
            commands::connect_platform,
            commands::disconnect_platform,
            commands::pick_folder,
            commands::pick_file,
            commands::analyze_migrations,
            commands::run_migration_script,
            commands::detect_migrations_dir,
            commands::start_migration_watch,
            executor::cancel::cancel_query,
            mcp::mcp_runtime_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
