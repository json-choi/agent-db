#![cfg(windows)]

use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::ptr;

use chrono::Utc;
use dopedb_protocol::{
    decode_frame, encode_frame, parse_frame_length, CommandName, RequestEnvelope, ResponseEnvelope,
    RuntimeDiscovery, StatusResult, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_MAX,
    PROTOCOL_MIN,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ServerOptions;
use uuid::Uuid;
use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_json_uses_the_owner_local_windows_named_pipe() {
    let temp = TempDir::new().unwrap();
    let runtime_directory = temp.path().join("runtime");
    fs::create_dir(&runtime_directory).unwrap();
    let runtime_id = Uuid::from_u128(1);
    let endpoint = format!(r"\\.\pipe\dopedb-{runtime_id}");
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&endpoint)
        .unwrap();
    let runtime_file = runtime_directory.join("runtime.json");
    let discovery = RuntimeDiscovery::new(
        runtime_id,
        std::process::id(),
        "0.3.3",
        PROTOCOL_MIN,
        PROTOCOL_MAX,
        &endpoint,
        Utc::now(),
    )
    .unwrap();
    fs::write(&runtime_file, serde_json::to_vec(&discovery).unwrap()).unwrap();
    restrict_owner_only(&runtime_file);

    let server_task = tokio::spawn(async move {
        server.connect().await.unwrap();
        let mut prefix = [0u8; 4];
        server.read_exact(&mut prefix).await.unwrap();
        let length = parse_frame_length(prefix, MAX_REQUEST_BYTES).unwrap();
        let mut frame = Vec::from(prefix);
        frame.resize(4 + length, 0);
        server.read_exact(&mut frame[4..]).await.unwrap();
        let request: RequestEnvelope = decode_frame(&frame, MAX_REQUEST_BYTES).unwrap();
        assert_eq!(request.command, CommandName::Status);
        let response = ResponseEnvelope::success(
            PROTOCOL_MAX,
            request.request_id,
            serde_json::to_value(StatusResult {
                app_version: "0.3.3".into(),
                protocol_min: PROTOCOL_MIN,
                protocol_max: PROTOCOL_MAX,
                runtime_id,
            })
            .unwrap(),
        );
        server
            .write_all(&encode_frame(&response, MAX_RESPONSE_BYTES).unwrap())
            .await
            .unwrap();
    });

    let output = Command::new(env!("CARGO_BIN_EXE_dopedb"))
        .args(["status", "--json"])
        .env("DOPEDB_RUNTIME_FILE", &runtime_file)
        .output()
        .unwrap();
    server_task.await.unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let status: StatusResult = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status.runtime_id, runtime_id);
}

fn restrict_owner_only(path: &Path) {
    let sddl = wide_null(OsStr::new("D:P(A;;GA;;;SY)(A;;GA;;;OW)"));
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        },
        0
    );
    let path = wide_null(path.as_os_str());
    let flags = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    assert_ne!(
        unsafe { SetFileSecurityW(path.as_ptr(), flags, descriptor) },
        0
    );
    unsafe {
        LocalFree(descriptor as HLOCAL);
    }
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
