use std::process::Command;
use std::time::{Duration, Instant};

use dopedb_protocol::{
    AppOpenArguments, AppOpenCommand, AppOpenResult, EmptyArguments, StatusCommand, StatusResult,
    VersionCommand, VersionResult,
};
use serde::Serialize;
use uuid::Uuid;

use crate::client::{BrokerClient, ClientError};
use crate::output::{self, OutputMode};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CliVersionOutput {
    cli_version: &'static str,
    app_version: String,
    protocol_min: u16,
    protocol_max: u16,
    command_schema_version: u16,
    runtime_id: Uuid,
}

pub(crate) async fn version(mode: OutputMode) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let result: VersionResult = client
        .request::<VersionCommand>(&EmptyArguments::default())
        .await?;
    if result.runtime_id != client.runtime_id() {
        return Err(ClientError::InvalidResponse);
    }
    let result = CliVersionOutput {
        cli_version: env!("CARGO_PKG_VERSION"),
        app_version: result.app_version,
        protocol_min: result.protocol_min,
        protocol_max: result.protocol_max,
        command_schema_version: result.command_schema_version,
        runtime_id: result.runtime_id,
    };
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human => output::write_human(&[
            format!("DopeDB CLI {}", result.cli_version),
            format!("DopeDB Desktop {}", result.app_version),
            format!("Protocol {}-{}", result.protocol_min, result.protocol_max),
            format!("Command schema {}", result.command_schema_version),
        ]),
    }
}

pub(crate) async fn status(mode: OutputMode) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let result: StatusResult = client
        .request::<StatusCommand>(&EmptyArguments::default())
        .await?;
    if result.runtime_id != client.runtime_id() {
        return Err(ClientError::InvalidResponse);
    }
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human => output::write_human(&[
            format!("DopeDB Desktop {} is running", result.app_version),
            format!("Protocol {}-{}", result.protocol_min, result.protocol_max),
        ]),
    }
}

pub(crate) async fn open(wait: bool, mode: OutputMode) -> Result<(), ClientError> {
    let mut launched = false;
    let client = match BrokerClient::discover() {
        Ok(client) => client,
        Err(ClientError::RuntimeUnavailable) => {
            launch_desktop()?;
            launched = true;
            if !wait {
                return write_open_result(
                    AppOpenResult {
                        runtime_id: None,
                        launched,
                        ready: false,
                    },
                    mode,
                );
            }
            wait_for_runtime(Duration::from_secs(15)).await?
        }
        Err(error) => return Err(error),
    };
    let mut result: AppOpenResult = client
        .request::<AppOpenCommand>(&AppOpenArguments { wait })
        .await?;
    if result.runtime_id != Some(client.runtime_id()) || !result.ready {
        return Err(ClientError::InvalidResponse);
    }
    result.launched = launched;
    write_open_result(result, mode)
}

fn write_open_result(result: AppOpenResult, mode: OutputMode) -> Result<(), ClientError> {
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human if result.ready => {
            output::write_human(&["DopeDB Desktop is ready".into()])
        }
        OutputMode::Human => output::write_human(&["DopeDB Desktop launch requested".into()]),
    }
}

async fn wait_for_runtime(timeout: Duration) -> Result<BrokerClient, ClientError> {
    let deadline = Instant::now() + timeout;
    loop {
        match BrokerClient::discover() {
            Ok(client) => return Ok(client),
            Err(ClientError::RuntimeUnavailable) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "macos")]
fn launch_desktop() -> Result<(), ClientError> {
    let status = Command::new("/usr/bin/open")
        .args(["-a", "DopeDB"])
        .status()
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(ClientError::RuntimeUnavailable)
    }
}

#[cfg(windows)]
fn launch_desktop() -> Result<(), ClientError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let candidate = windows_desktop_candidates()
        .into_iter()
        .find(|path| path.is_file())
        .ok_or(ClientError::RuntimeUnavailable)?;
    let status = Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(candidate)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(ClientError::RuntimeUnavailable)
    }
}

#[cfg(windows)]
fn windows_desktop_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if executable.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .eq_ignore_ascii_case("dopedb-cli.exe")
        }) {
            if let Some(directory) = executable.parent() {
                candidates.push(directory.join("dopedb.exe"));
            }
        }
    }
    if let Some(local) = dirs::data_local_dir() {
        candidates.push(local.join("DopeDB").join("dopedb.exe"));
        candidates.push(local.join("Programs").join("DopeDB").join("dopedb.exe"));
    }
    candidates.dedup();
    candidates
}

#[cfg(not(any(target_os = "macos", windows)))]
fn launch_desktop() -> Result<(), ClientError> {
    let status = Command::new("dopedb-desktop")
        .status()
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(ClientError::RuntimeUnavailable)
    }
}
