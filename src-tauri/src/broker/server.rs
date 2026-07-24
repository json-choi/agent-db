//! Owner-local UDS/named-pipe server with bounded length-prefixed frames.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use dopedb_protocol::{
    decode_frame, encode_frame, parse_frame_length, RequestEnvelope, RuntimeDiscovery,
    MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_MAX, PROTOCOL_MIN,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
#[cfg(all(test, unix))]
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::services::ApplicationServices;

use super::dispatch::BrokerDispatcher;
use super::{discovery, peer, BrokerRuntime};

const CONTROL_IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

pub(crate) async fn serve(
    runtime: BrokerRuntime,
    services: ApplicationServices,
    app_handle: tauri::AppHandle,
) -> AppResult<()> {
    match discovery::default_runtime_file() {
        Ok(runtime_file) => serve_at(runtime, runtime_file, Some(services), Some(app_handle)).await,
        Err(error) => {
            runtime.finish(Some(&error));
            Err(error)
        }
    }
}

async fn serve_at(
    runtime: BrokerRuntime,
    runtime_file: PathBuf,
    services: Option<ApplicationServices>,
    app_handle: Option<tauri::AppHandle>,
) -> AppResult<()> {
    let result = platform_serve(&runtime, &runtime_file, services, app_handle).await;
    discovery::remove_if_owned(&runtime_file, runtime.runtime_id());
    runtime.finish(result.as_ref().err());
    result
}

#[cfg(unix)]
async fn platform_serve(
    runtime: &BrokerRuntime,
    runtime_file: &Path,
    services: Option<ApplicationServices>,
    app_handle: Option<tauri::AppHandle>,
) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let directory = discovery::prepare_runtime_directory(runtime_file)?;
    let endpoint = directory.join(format!(
        "broker-{}.sock",
        &runtime.runtime_id().simple().to_string()[..16]
    ));
    if std::fs::symlink_metadata(&endpoint).is_ok() {
        std::fs::remove_file(&endpoint)?;
    }
    let listener = tokio::net::UnixListener::bind(&endpoint)?;
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))?;
    let endpoint_text = endpoint.to_string_lossy().into_owned();
    publish_discovery(runtime, runtime_file, &endpoint_text)?;
    runtime.mark_running(endpoint_text, runtime_file.to_path_buf());

    let dispatcher = BrokerDispatcher::new(
        runtime.runtime_id(),
        env!("CARGO_PKG_VERSION"),
        runtime.sessions().clone(),
        services,
        app_handle,
    );
    let mut tasks = JoinSet::new();
    let connection_slots = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let loop_result = loop {
        tokio::select! {
            _ = runtime.shutdown_token().cancelled() => break Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(AppError::Io(error)),
                };
                if let Err(error) = peer::verify_unix_peer(&stream) {
                    tracing::warn!(error_kind = ?error.kind(), "rejected local broker peer");
                    continue;
                }
                let permit = match Arc::clone(&connection_slots).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!("rejected local broker peer because the connection limit is full");
                        continue;
                    }
                };
                let dispatcher = dispatcher.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_stream(stream, dispatcher).await {
                        tracing::debug!(error_kind = ?error.kind(), "broker connection closed");
                    }
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    drop(listener);
    let _ = std::fs::remove_file(endpoint);
    loop_result
}

#[cfg(windows)]
async fn platform_serve(
    runtime: &BrokerRuntime,
    runtime_file: &Path,
    services: Option<ApplicationServices>,
    app_handle: Option<tauri::AppHandle>,
) -> AppResult<()> {
    let _directory = discovery::prepare_runtime_directory(runtime_file)?;
    let endpoint = format!(r"\\.\pipe\dopedb-{}", runtime.runtime_id());
    let mut server = peer::create_named_pipe(&endpoint, true)?;
    publish_discovery(runtime, runtime_file, &endpoint)?;
    runtime.mark_running(endpoint.clone(), runtime_file.to_path_buf());

    let dispatcher = BrokerDispatcher::new(
        runtime.runtime_id(),
        env!("CARGO_PKG_VERSION"),
        runtime.sessions().clone(),
        services,
        app_handle,
    );
    let mut tasks = JoinSet::new();
    let connection_slots = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let loop_result = loop {
        tokio::select! {
            _ = runtime.shutdown_token().cancelled() => break Ok(()),
            connected = server.connect() => {
                if let Err(error) = connected {
                    break Err(AppError::Io(error));
                }
                let next = match peer::create_named_pipe(&endpoint, false) {
                    Ok(next) => next,
                    Err(error) => break Err(AppError::Io(error)),
                };
                let connected = std::mem::replace(&mut server, next);
                if let Err(error) = peer::verify_named_pipe_peer(&connected) {
                    tracing::warn!(error_kind = ?error.kind(), "rejected local broker peer");
                    continue;
                }
                let permit = match Arc::clone(&connection_slots).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!("rejected local broker peer because the connection limit is full");
                        continue;
                    }
                };
                let dispatcher = dispatcher.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_stream(connected, dispatcher).await {
                        tracing::debug!(error_kind = ?error.kind(), "broker connection closed");
                    }
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    loop_result
}

fn publish_discovery(
    runtime: &BrokerRuntime,
    runtime_file: &Path,
    endpoint: &str,
) -> AppResult<()> {
    let metadata = RuntimeDiscovery::new(
        runtime.runtime_id(),
        std::process::id(),
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_MIN,
        PROTOCOL_MAX,
        endpoint,
        Utc::now(),
    )
    .map_err(|_| AppError::Config("could not construct runtime discovery metadata".into()))?;
    discovery::publish(runtime_file, &metadata)
}

async fn handle_stream<S>(mut stream: S, dispatcher: BrokerDispatcher) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = tokio::time::timeout(CONTROL_IO_TIMEOUT, read_request(&mut stream))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "broker read timed out")
        })??;
    let response = dispatcher.dispatch(request).await;
    let frame = encode_frame(&response, MAX_RESPONSE_BYTES)
        .map_err(|_| std::io::Error::other("broker response framing failed"))?;
    tokio::time::timeout(CONTROL_IO_TIMEOUT, stream.write_all(&frame))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "broker write timed out")
        })??;
    stream.shutdown().await
}

async fn read_request<S>(stream: &mut S) -> std::io::Result<RequestEnvelope>
where
    S: AsyncRead + Unpin,
{
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix).await?;
    let length = parse_frame_length(prefix, MAX_REQUEST_BYTES).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid frame length")
    })?;
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + length, 0);
    stream.read_exact(&mut frame[4..]).await?;
    decode_frame(&frame, MAX_REQUEST_BYTES)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid request frame"))
}

#[cfg(all(test, unix))]
mod tests {
    use dopedb_protocol::{
        CommandName, EmptyArguments, RequestEnvelope, ResponseEnvelope, StatusResult,
        COMMAND_SCHEMA_VERSION,
    };
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn unix_server_publishes_status_and_cleans_discovery_on_shutdown() {
        let temp = TempDir::new().unwrap();
        let runtime_file = temp.path().join("runtime").join("runtime.json");
        let runtime = BrokerRuntime::new(Uuid::new_v4());
        assert!(runtime.prepare_start());
        let task_runtime = runtime.clone();
        let task_file = runtime_file.clone();
        let task = tokio::spawn(async move { serve_at(task_runtime, task_file, None, None).await });

        let metadata = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(bytes) = tokio::fs::read(&runtime_file).await {
                    if let Ok(metadata) = serde_json::from_slice::<RuntimeDiscovery>(&bytes) {
                        break metadata;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut stream = tokio::net::UnixStream::connect(metadata.endpoint())
            .await
            .unwrap();
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_MAX,
            command_schema_version: COMMAND_SCHEMA_VERSION,
            request_id: Uuid::new_v4(),
            authentication: None,
            command: CommandName::Status,
            arguments: serde_json::to_value(EmptyArguments::default()).unwrap(),
        };
        stream
            .write_all(&encode_frame(&request, MAX_REQUEST_BYTES).unwrap())
            .await
            .unwrap();
        let mut prefix = [0u8; 4];
        stream.read_exact(&mut prefix).await.unwrap();
        let length = parse_frame_length(prefix, MAX_RESPONSE_BYTES).unwrap();
        let mut frame = Vec::from(prefix);
        frame.resize(4 + length, 0);
        stream.read_exact(&mut frame[4..]).await.unwrap();
        let response: ResponseEnvelope = decode_frame(&frame, MAX_RESPONSE_BYTES).unwrap();
        let status: StatusResult =
            serde_json::from_value(response.result().cloned().unwrap()).unwrap();
        assert_eq!(status.runtime_id, runtime.runtime_id());

        runtime.shutdown();
        task.await.unwrap().unwrap();
        assert!(!runtime_file.exists());
    }
}
