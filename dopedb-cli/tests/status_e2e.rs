#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::thread;

use chrono::Utc;
use dopedb_protocol::{
    decode_frame, encode_frame, parse_frame_length, CommandName, RequestEnvelope, ResponseEnvelope,
    RuntimeDiscovery, StatusResult, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_MAX,
    PROTOCOL_MIN,
};
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn status_json_discovers_and_calls_the_owner_local_runtime() {
    let temp = TempDir::new().unwrap();
    let runtime_directory = temp.path().join("runtime");
    fs::create_dir(&runtime_directory).unwrap();
    fs::set_permissions(&runtime_directory, fs::Permissions::from_mode(0o700)).unwrap();
    let runtime_id = Uuid::from_u128(1);
    let runtime_id_text = runtime_id.simple().to_string();
    let endpoint = runtime_directory.join(format!("broker-{}.sock", &runtime_id_text[..16]));
    let listener = UnixListener::bind(&endpoint).unwrap();
    fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
    let runtime_file = runtime_directory.join("runtime.json");
    let discovery = RuntimeDiscovery::new(
        runtime_id,
        std::process::id(),
        "0.3.3",
        PROTOCOL_MIN,
        PROTOCOL_MAX,
        endpoint.to_string_lossy(),
        Utc::now(),
    )
    .unwrap();
    fs::write(&runtime_file, serde_json::to_vec(&discovery).unwrap()).unwrap();
    fs::set_permissions(&runtime_file, fs::Permissions::from_mode(0o600)).unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut prefix = [0u8; 4];
        stream.read_exact(&mut prefix).unwrap();
        let length = parse_frame_length(prefix, MAX_REQUEST_BYTES).unwrap();
        let mut frame = Vec::from(prefix);
        frame.resize(4 + length, 0);
        stream.read_exact(&mut frame[4..]).unwrap();
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
        stream
            .write_all(&encode_frame(&response, MAX_RESPONSE_BYTES).unwrap())
            .unwrap();
    });

    let output = Command::new(env!("CARGO_BIN_EXE_dopedb"))
        .args(["status", "--json"])
        .env("DOPEDB_RUNTIME_FILE", &runtime_file)
        .output()
        .unwrap();
    server.join().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let status: StatusResult = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status.runtime_id, runtime_id);
    assert_eq!(status.app_version, "0.3.3");
}

#[test]
fn missing_runtime_has_stable_exit_code_and_keeps_stdout_clean() {
    let temp = TempDir::new().unwrap();
    let runtime_file = temp.path().join("missing-runtime.json");
    let output = Command::new(env!("CARGO_BIN_EXE_dopedb"))
        .args(["status", "--json"])
        .env("DOPEDB_RUNTIME_FILE", runtime_file)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "the DopeDB Desktop runtime is unavailable; open the app and try again\n"
    );
}

#[test]
fn protocol_mismatch_has_stable_exit_code_and_explanation() {
    let temp = TempDir::new().unwrap();
    let runtime_directory = temp.path().join("runtime");
    fs::create_dir(&runtime_directory).unwrap();
    fs::set_permissions(&runtime_directory, fs::Permissions::from_mode(0o700)).unwrap();
    let runtime_id = Uuid::from_u128(2);
    let runtime_id_text = runtime_id.simple().to_string();
    let endpoint = runtime_directory.join(format!("broker-{}.sock", &runtime_id_text[..16]));
    let _listener = UnixListener::bind(&endpoint).unwrap();
    fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
    let runtime_file = runtime_directory.join("runtime.json");
    let incompatible = PROTOCOL_MAX + 1;
    let discovery = RuntimeDiscovery::new(
        runtime_id,
        std::process::id(),
        "99.0.0",
        incompatible,
        incompatible,
        endpoint.to_string_lossy(),
        Utc::now(),
    )
    .unwrap();
    fs::write(&runtime_file, serde_json::to_vec(&discovery).unwrap()).unwrap();
    fs::set_permissions(&runtime_file, fs::Permissions::from_mode(0o600)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_dopedb"))
        .args(["status", "--json"])
        .env("DOPEDB_RUNTIME_FILE", runtime_file)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(9));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "the CLI and Desktop runtime protocols are incompatible\n"
    );
}
