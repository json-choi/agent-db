//! dopedb — Rust core entrypoint. Wires modules, state, and the Tauri command
//! surface. Agents drive the DB through the local MCP server (see the `mcp` module).

mod agent;
mod audit;
mod commands;
mod connection;
mod dashboard;
mod driver;
mod error;
mod executor;
mod introspect;
mod mcp;
pub mod model;
mod mongo;
mod monitoring;
mod safety;
mod sql_script;
mod state;
mod store;
pub mod workspace;
mod workspace_auth;

pub use error::{AppError, AppResult};

use std::time::Duration;

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
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .setup(|app| {
            // Start the local MCP server (Streamable HTTP on 127.0.0.1:7686). It shares
            // the app's Store + credential-store + safety pipeline via the tools in `mcp`.
            let st = app.state::<state::AppState>();
            let store = st.store.clone();
            let token = st.mcp_token.clone();
            // Share the SAME live-connection map + runtime-status cell as AppState, so
            // connection edits/deletes evict the agent's pools and the UI can tell
            // "actually listening" from "config exists".
            let conns = st.connections.clone();
            let runtime = st.mcp_runtime.clone();
            let plans = mcp::query_plan_store();
            let handle = app.handle().clone();
            // HTTP endpoint (Claude Code / Cursor / …).
            {
                let (store, token, handle, conns, plans, runtime) = (
                    store.clone(),
                    token.clone(),
                    handle.clone(),
                    conns.clone(),
                    plans.clone(),
                    runtime.clone(),
                );
                tauri::async_runtime::spawn(async move {
                    if let Err(e) =
                        mcp::serve_mcp(handle, store, token, conns, plans, runtime).await
                    {
                        tracing::error!("MCP HTTP server failed: {e}");
                    }
                });
            }
            // Raw TCP listener the stdio bridge dials (Claude Desktop).
            tauri::async_runtime::spawn(async move {
                if let Err(e) =
                    mcp::serve_stdio_bridge(handle, store, token, conns, plans, runtime).await
                {
                    tracing::error!("MCP stdio-bridge failed: {e}");
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::workspace_feature_state,
            commands::workspace_auth_state,
            commands::workspace_sign_out,
            commands::begin_workspace_login,
            commands::poll_workspace_login,
            commands::workspace_console_url,
            commands::list_workspaces,
            commands::refresh_workspace_memberships,
            commands::get_active_workspace,
            commands::set_active_workspace,
            commands::copy_connection_to_workspace,
            commands::bind_workspace_connection_credentials,
            commands::list_connections,
            commands::list_drivers,
            commands::install_driver,
            commands::upsert_connection,
            commands::set_connection_schema_group,
            commands::set_connections_schema_group,
            commands::delete_connection,
            commands::test_connection,
            commands::test_connection_profile,
            commands::list_dashboards,
            commands::save_dashboard,
            commands::delete_dashboard,
            commands::run_dashboard,
            commands::get_schema,
            commands::refresh_schema,
            introspect::get_table_ddl,
            commands::classify_sql,
            commands::preview_sql,
            commands::run_sql,
            commands::run_document_query,
            commands::run_script,
            commands::get_safety,
            commands::set_safety,
            commands::get_monitoring_status,
            commands::set_postgres_monitoring,
            commands::audit_verify,
            commands::audit_snapshot,
            commands::list_history,
            commands::mcp_status,
            commands::mcp_platforms,
            commands::connect_platform,
            commands::disconnect_platform,
            commands::open_agent_app,
            commands::pick_file,
            commands::detect_agent_clis,
            commands::list_agent_models,
            commands::list_chat_threads,
            commands::get_chat_messages,
            commands::create_chat_thread,
            commands::delete_chat_thread,
            commands::send_chat_turn,
            executor::cancel::cancel_query,
            mcp::mcp_runtime_status,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // A chat turn's CLI child is `kill_on_drop`, but that only fires once its
            // future is actually dropped by the tokio task polling it — not the instant
            // `cancel()` sends the signal. Sending the signal and returning immediately
            // races the process's own exit (this callback runs right before the process
            // is torn down), so the child can be orphaned. Block here (bounded) until
            // the turn has actually wound down, so the child is reaped first.
            if let tauri::RunEvent::Exit = event {
                let chat = app_handle.state::<state::AppState>().chat.clone();
                if let Some(turn_id) = chat.active_turn() {
                    executor::cancel::cancel(turn_id);
                    tauri::async_runtime::block_on(chat.wait_idle(Duration::from_secs(5)));
                }
            }
        });
}
