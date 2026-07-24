use std::fmt;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dopedb_protocol::{
    decode_frame, encode_frame, negotiate_protocol, parse_frame_length, AuthenticationRequirement,
    CommandSpec, ProtocolError, RequestEnvelope, ResponseEnvelope, RuntimeDiscovery,
    SessionAuthentication, COMMAND_SCHEMA_VERSION, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    PROTOCOL_MAX, PROTOCOL_MIN, RUNTIME_DIRECTORY_NAME, RUNTIME_FILE_NAME,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

const DISCOVERY_MAX_BYTES: u64 = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_DATABASE_TIMEOUT: Duration = Duration::from_secs(30);
// The runtime's shared query executor has a 300-second wall-clock ceiling.
// Keep the transport deadline slightly outside it so the broker can return the
// stable timeout/cancel result instead of the client misclassifying it as a dead
// runtime while target cleanup is still in progress.
const CONTROL_QUERY_RUN_TIMEOUT: Duration = Duration::from_secs(305);
const CONTROL_OPERATION_WAIT_TIMEOUT: Duration = Duration::from_secs(35);

pub(crate) struct BrokerClient {
    discovery: RuntimeDiscovery,
    protocol_version: u16,
}

impl BrokerClient {
    pub(crate) fn discover() -> Result<Self, ClientError> {
        let runtime_file = runtime_file_path()?;
        let discovery = read_discovery(&runtime_file)?;
        if !process_is_alive(discovery.pid()) {
            remove_stale_discovery(&runtime_file, discovery.runtime_id());
            return Err(ClientError::RuntimeUnavailable);
        }
        validate_endpoint(&runtime_file, &discovery)?;
        let protocol_version = negotiate_protocol(
            discovery.protocol_min(),
            discovery.protocol_max(),
            PROTOCOL_MIN,
            PROTOCOL_MAX,
        )
        .map_err(|_| ClientError::ProtocolMismatch)?;
        Ok(Self {
            discovery,
            protocol_version,
        })
    }

    pub(crate) fn runtime_id(&self) -> Uuid {
        self.discovery.runtime_id()
    }

    pub(crate) async fn request<C>(
        &self,
        arguments: &C::Arguments,
    ) -> Result<C::Result, ClientError>
    where
        C: CommandSpec,
    {
        let authentication = match C::AUTHENTICATION {
            AuthenticationRequirement::None => None,
            AuthenticationRequirement::TerminalSession => Some(session_authentication()?),
        };
        let request = RequestEnvelope {
            protocol_version: self.protocol_version,
            command_schema_version: COMMAND_SCHEMA_VERSION,
            request_id: Uuid::new_v4(),
            authentication,
            command: C::NAME,
            arguments: serde_json::to_value(arguments).map_err(|_| ClientError::Internal)?,
        };
        let request_id = request.request_id;
        let response = exchange(
            self.discovery.endpoint(),
            &request,
            response_timeout(C::NAME),
        )
        .await?;
        if response.request_id() != request_id
            || response.protocol_version() != self.protocol_version
        {
            return Err(ClientError::InvalidResponse);
        }
        if let Some(error) = response.error() {
            return Err(ClientError::Remote(error.clone()));
        }
        serde_json::from_value(
            response
                .result()
                .cloned()
                .ok_or(ClientError::InvalidResponse)?,
        )
        .map_err(|_| ClientError::InvalidResponse)
    }
}

fn response_timeout(command: dopedb_protocol::CommandName) -> Duration {
    use dopedb_protocol::CommandName;

    match command {
        CommandName::ConnectionTest
        | CommandName::CatalogShow
        | CommandName::SchemaList
        | CommandName::TableDescribe
        | CommandName::QueryPlan
        | CommandName::SqlPropose => CONTROL_DATABASE_TIMEOUT,
        CommandName::QueryRun => CONTROL_QUERY_RUN_TIMEOUT,
        CommandName::OperationWait => CONTROL_OPERATION_WAIT_TIMEOUT,
        _ => CONTROL_METADATA_TIMEOUT,
    }
}

fn remove_stale_discovery(runtime_file: &Path, runtime_id: Uuid) {
    let Ok(discovery) = read_discovery(runtime_file) else {
        return;
    };
    if discovery.runtime_id() == runtime_id && !process_is_alive(discovery.pid()) {
        let _ = fs::remove_file(runtime_file);
    }
}

pub(crate) enum ClientError {
    InvalidArguments,
    RuntimeUnavailable,
    AuthenticationUnavailable,
    ConnectionNotFound,
    AmbiguousConnection(Vec<Uuid>),
    ProtocolMismatch,
    InvalidResponse,
    Remote(ProtocolError),
    Internal,
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments => formatter.write_str(
                "the command arguments are invalid; use --help for the supported syntax",
            ),
            Self::RuntimeUnavailable => formatter
                .write_str("the DopeDB Desktop runtime is unavailable; open the app and try again"),
            Self::AuthenticationUnavailable => {
                formatter.write_str("this command requires an approved in-app Terminal session")
            }
            Self::ConnectionNotFound => {
                formatter.write_str("no connection matches the exact selector")
            }
            Self::AmbiguousConnection(candidates) => {
                formatter.write_str("the connection name is ambiguous; use ")?;
                for (index, id) in candidates.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "id:{id}")?;
                }
                Ok(())
            }
            Self::ProtocolMismatch => {
                formatter.write_str("the CLI and Desktop runtime protocols are incompatible")
            }
            Self::InvalidResponse => {
                formatter.write_str("the Desktop runtime returned an invalid response")
            }
            Self::Remote(error) => formatter.write_str(error.message()),
            Self::Internal => formatter.write_str("the DopeDB CLI encountered an internal error"),
        }
    }
}

impl fmt::Debug for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Remote(error) => formatter.debug_tuple("Remote").field(error).finish(),
            Self::InvalidArguments => formatter.write_str("InvalidArguments"),
            Self::RuntimeUnavailable => formatter.write_str("RuntimeUnavailable"),
            Self::AuthenticationUnavailable => formatter.write_str("AuthenticationUnavailable"),
            Self::ConnectionNotFound => formatter.write_str("ConnectionNotFound"),
            Self::AmbiguousConnection(candidates) => formatter
                .debug_struct("AmbiguousConnection")
                .field("candidate_count", &candidates.len())
                .finish(),
            Self::ProtocolMismatch => formatter.write_str("ProtocolMismatch"),
            Self::InvalidResponse => formatter.write_str("InvalidResponse"),
            Self::Internal => formatter.write_str("Internal"),
        }
    }
}

fn runtime_file_path() -> Result<PathBuf, ClientError> {
    if let Some(path) = std::env::var_os("DOPEDB_RUNTIME_FILE") {
        if path.is_empty() {
            return Err(ClientError::RuntimeUnavailable);
        }
        return Ok(PathBuf::from(path));
    }
    let base = dirs::data_dir().ok_or(ClientError::RuntimeUnavailable)?;
    Ok(base
        .join("dopedb")
        .join(RUNTIME_DIRECTORY_NAME)
        .join(RUNTIME_FILE_NAME))
}

fn read_discovery(runtime_file: &Path) -> Result<RuntimeDiscovery, ClientError> {
    let symlink =
        fs::symlink_metadata(runtime_file).map_err(|_| ClientError::RuntimeUnavailable)?;
    if symlink.file_type().is_symlink()
        || !symlink.is_file()
        || symlink.len() == 0
        || symlink.len() > DISCOVERY_MAX_BYTES
    {
        return Err(ClientError::RuntimeUnavailable);
    }
    let file = open_discovery_no_follow(runtime_file)?;
    validate_discovery_permissions(&file)?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(symlink.len()).map_err(|_| ClientError::RuntimeUnavailable)?,
    );
    file.take(DISCOVERY_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    if bytes.is_empty() || bytes.len() as u64 > DISCOVERY_MAX_BYTES {
        return Err(ClientError::RuntimeUnavailable);
    }
    let discovery: RuntimeDiscovery =
        serde_json::from_slice(&bytes).map_err(|_| ClientError::RuntimeUnavailable)?;
    discovery
        .validate()
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    Ok(discovery)
}

#[cfg(unix)]
fn open_discovery_no_follow(path: &Path) -> Result<File, ClientError> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| ClientError::RuntimeUnavailable)
}

#[cfg(windows)]
fn open_discovery_no_follow(path: &Path) -> Result<File, ClientError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| ClientError::RuntimeUnavailable)
}

#[cfg(unix)]
fn validate_discovery_permissions(file: &File) -> Result<(), ClientError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = file
        .metadata()
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o077 != 0 {
        return Err(ClientError::RuntimeUnavailable);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_discovery_permissions(file: &File) -> Result<(), ClientError> {
    windows_security::validate_owner_only_file(file)
}

#[cfg(unix)]
fn validate_endpoint(runtime_file: &Path, discovery: &RuntimeDiscovery) -> Result<(), ClientError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    let runtime_directory = runtime_file
        .parent()
        .ok_or(ClientError::RuntimeUnavailable)?;
    let directory_metadata =
        fs::symlink_metadata(runtime_directory).map_err(|_| ClientError::RuntimeUnavailable)?;
    if !directory_metadata.file_type().is_dir()
        || directory_metadata.uid() != unsafe { libc::geteuid() }
        || directory_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ClientError::RuntimeUnavailable);
    }
    let endpoint = Path::new(discovery.endpoint());
    let runtime_id = discovery.runtime_id().simple().to_string();
    let expected_name = format!("broker-{}.sock", &runtime_id[..16]);
    if !endpoint.is_absolute()
        || endpoint.parent() != Some(runtime_directory)
        || !endpoint
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == expected_name)
    {
        return Err(ClientError::RuntimeUnavailable);
    }
    let metadata = fs::symlink_metadata(endpoint).map_err(|_| ClientError::RuntimeUnavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ClientError::RuntimeUnavailable);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_endpoint(
    _runtime_file: &Path,
    discovery: &RuntimeDiscovery,
) -> Result<(), ClientError> {
    let expected = format!(r"\\.\pipe\dopedb-{}", discovery.runtime_id());
    if discovery.endpoint() == expected {
        Ok(())
    } else {
        Err(ClientError::RuntimeUnavailable)
    }
}

fn session_authentication() -> Result<SessionAuthentication, ClientError> {
    let session_id = std::env::var("DOPEDB_TERMINAL_SESSION_ID")
        .ok()
        .and_then(|value| Uuid::parse_str(&value).ok())
        .ok_or(ClientError::AuthenticationUnavailable)?;
    let token = std::env::var("DOPEDB_SESSION_TOKEN")
        .map_err(|_| ClientError::AuthenticationUnavailable)?;
    if token.is_empty() {
        return Err(ClientError::AuthenticationUnavailable);
    }
    Ok(SessionAuthentication::new(session_id, token))
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let mut exit_code = 0u32;
    let alive = unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0
        && exit_code == STILL_ACTIVE.cast_unsigned();
    unsafe {
        CloseHandle(process);
    }
    alive
}

#[cfg(windows)]
mod windows_security {
    use std::ffi::c_void;
    use std::fs::File;
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        EqualSid, GetAce, GetSecurityDescriptorControl, GetTokenInformation, IsWellKnownSid,
        TokenUser, WinCreatorOwnerRightsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER,
        ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_TAG_INFO,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::ClientError;

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const INHERITED_ACE: u8 = 0x10;

    pub(super) fn validate_owner_only_file(file: &File) -> Result<(), ClientError> {
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast::<c_void>(),
                u32::try_from(size_of::<FILE_ATTRIBUTE_TAG_INFO>())
                    .map_err(|_| ClientError::RuntimeUnavailable)?,
            )
        } == 0
            || tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(ClientError::RuntimeUnavailable);
        }

        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 || descriptor.is_null() || owner.is_null() || dacl.is_null() {
            return Err(ClientError::RuntimeUnavailable);
        }
        let _descriptor = SecurityDescriptor(descriptor);

        let current_user = current_user()?;
        if unsafe { EqualSid(owner, current_user.sid()) } == 0 {
            return Err(ClientError::RuntimeUnavailable);
        }

        let mut control = 0u16;
        let mut revision = 0u32;
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return Err(ClientError::RuntimeUnavailable);
        }

        let ace_count = unsafe { (*dacl).AceCount };
        if !(1..=3).contains(&ace_count) {
            return Err(ClientError::RuntimeUnavailable);
        }
        let mut owner_access = false;
        for index in 0..u32::from(ace_count) {
            let mut raw_ace = ptr::null_mut::<c_void>();
            if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 || raw_ace.is_null() {
                return Err(ClientError::RuntimeUnavailable);
            }
            let header = unsafe { &*raw_ace.cast::<ACE_HEADER>() };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags & INHERITED_ACE != 0 {
                return Err(ClientError::RuntimeUnavailable);
            }
            if usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
                return Err(ClientError::RuntimeUnavailable);
            }
            let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = (&raw const ace.SidStart).cast_mut().cast::<c_void>();
            let is_owner = unsafe { EqualSid(sid, current_user.sid()) } != 0
                || unsafe { IsWellKnownSid(sid, WinCreatorOwnerRightsSid) } != 0;
            let is_system = unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } != 0;
            if !is_owner && !is_system {
                return Err(ClientError::RuntimeUnavailable);
            }
            owner_access |= is_owner;
        }
        if !owner_access {
            return Err(ClientError::RuntimeUnavailable);
        }
        Ok(())
    }

    fn current_user() -> Result<TokenUserBuffer, ClientError> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(ClientError::RuntimeUnavailable);
        }
        let token = OwnedHandle(token);
        TokenUserBuffer::read(token.0)
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0 as HLOCAL);
                }
            }
        }
    }

    struct TokenUserBuffer {
        words: Vec<usize>,
    }

    impl TokenUserBuffer {
        fn read(token: HANDLE) -> Result<Self, ClientError> {
            let mut required = 0u32;
            unsafe {
                GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
            }
            if required < size_of::<TOKEN_USER>() as u32 {
                return Err(ClientError::RuntimeUnavailable);
            }
            let word_bytes = size_of::<usize>();
            let words = usize::try_from(required)
                .ok()
                .and_then(|bytes| bytes.checked_add(word_bytes - 1))
                .map(|bytes| bytes / word_bytes)
                .ok_or(ClientError::RuntimeUnavailable)?;
            let mut buffer = Self {
                words: vec![0usize; words],
            };
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.words.as_mut_ptr().cast::<c_void>(),
                    required,
                    &mut required,
                )
            } == 0
            {
                return Err(ClientError::RuntimeUnavailable);
            }
            Ok(buffer)
        }

        fn sid(&self) -> PSID {
            let user = unsafe { &*self.words.as_ptr().cast::<TOKEN_USER>() };
            user.User.Sid
        }
    }
}

#[cfg(unix)]
async fn exchange(
    endpoint: &str,
    request: &RequestEnvelope,
    response_timeout: Duration,
) -> Result<ResponseEnvelope, ClientError> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::UnixStream::connect(endpoint))
        .await
        .map_err(|_| ClientError::RuntimeUnavailable)?
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    exchange_stream(stream, request, response_timeout).await
}

#[cfg(windows)]
async fn exchange(
    endpoint: &str,
    request: &RequestEnvelope,
    response_timeout: Duration,
) -> Result<ResponseEnvelope, ClientError> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let endpoint = endpoint.to_owned();
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, async move {
        loop {
            match ClientOptions::new().open(&endpoint) {
                Ok(stream) => return Ok(stream),
                Err(error) if error.raw_os_error() == Some(231) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| ClientError::RuntimeUnavailable)?
    .map_err(|_| ClientError::RuntimeUnavailable)?;
    exchange_stream(stream, request, response_timeout).await
}

async fn exchange_stream<S>(
    mut stream: S,
    request: &RequestEnvelope,
    response_timeout: Duration,
) -> Result<ResponseEnvelope, ClientError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = encode_request(request)?;
    tokio::time::timeout(CONTROL_WRITE_TIMEOUT, stream.write_all(&frame))
        .await
        .map_err(|_| ClientError::RuntimeUnavailable)?
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    let response_deadline = tokio::time::Instant::now() + response_timeout;
    let mut prefix = [0u8; 4];
    tokio::time::timeout_at(response_deadline, stream.read_exact(&mut prefix))
        .await
        .map_err(|_| ClientError::RuntimeUnavailable)?
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    let length =
        parse_frame_length(prefix, MAX_RESPONSE_BYTES).map_err(|_| ClientError::InvalidResponse)?;
    let mut response = Vec::from(prefix);
    response.resize(4 + length, 0);
    tokio::time::timeout_at(response_deadline, stream.read_exact(&mut response[4..]))
        .await
        .map_err(|_| ClientError::RuntimeUnavailable)?
        .map_err(|_| ClientError::RuntimeUnavailable)?;
    decode_frame(&response, MAX_RESPONSE_BYTES).map_err(|_| ClientError::InvalidResponse)
}

fn encode_request(request: &RequestEnvelope) -> Result<Vec<u8>, ClientError> {
    encode_frame(request, MAX_REQUEST_BYTES).map_err(|_| ClientError::InvalidArguments)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use chrono::Utc;
    #[cfg(unix)]
    use tempfile::TempDir;

    use super::*;

    #[cfg(unix)]
    fn discovery(runtime_id: Uuid, endpoint: &str) -> RuntimeDiscovery {
        RuntimeDiscovery::new(
            runtime_id,
            std::process::id(),
            "0.3.3",
            PROTOCOL_MIN,
            PROTOCOL_MAX,
            endpoint,
            Utc::now(),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_group_readable_files_and_redirected_endpoints() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let runtime_file = temp.path().join("runtime.json");
        let runtime_id = Uuid::from_u128(1);
        let runtime_id_text = runtime_id.simple().to_string();
        let endpoint = temp
            .path()
            .join(format!("broker-{}.sock", &runtime_id_text[..16]));
        std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
        fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
        let value = discovery(runtime_id, endpoint.to_str().unwrap());
        fs::write(&runtime_file, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::set_permissions(&runtime_file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            read_discovery(&runtime_file),
            Err(ClientError::RuntimeUnavailable)
        ));

        fs::set_permissions(&runtime_file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_endpoint(&runtime_file, &value).is_err());
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_endpoint(&runtime_file, &value).is_ok());
        let redirected = discovery(runtime_id, "/tmp/broker-redirect.sock");
        fs::write(&runtime_file, serde_json::to_vec(&redirected).unwrap()).unwrap();
        assert!(validate_endpoint(&runtime_file, &redirected).is_err());
    }

    #[test]
    fn client_error_debug_and_display_never_echo_environment_secrets() {
        let secret = "fixture-session-secret";
        let error = ClientError::AuthenticationUnavailable;
        assert!(!format!("{error:?}").contains(secret));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn command_timeouts_cover_database_and_operation_wait_budgets() {
        assert_eq!(
            response_timeout(dopedb_protocol::CommandName::Status),
            CONTROL_METADATA_TIMEOUT
        );
        assert_eq!(
            response_timeout(dopedb_protocol::CommandName::QueryRun),
            CONTROL_QUERY_RUN_TIMEOUT
        );
        assert!(CONTROL_QUERY_RUN_TIMEOUT > Duration::from_secs(300));
        assert!(
            response_timeout(dopedb_protocol::CommandName::OperationWait) > Duration::from_secs(30)
        );
    }

    #[test]
    fn oversized_outbound_payload_is_a_usage_error_before_runtime_io() {
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_MAX,
            command_schema_version: COMMAND_SCHEMA_VERSION,
            request_id: Uuid::new_v4(),
            authentication: None,
            command: dopedb_protocol::CommandName::QueryPlan,
            arguments: serde_json::json!({
                "connection": "current",
                "sql": "x".repeat(dopedb_protocol::MAX_STRING_BYTES + 1)
            }),
        };
        assert!(matches!(
            encode_request(&request),
            Err(ClientError::InvalidArguments)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn dead_runtime_discovery_is_removed_only_when_its_identity_still_matches() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let runtime_file = temp.path().join("runtime.json");
        let stale = RuntimeDiscovery::new(
            Uuid::from_u128(1),
            u32::MAX,
            "0.3.3",
            PROTOCOL_MIN,
            PROTOCOL_MAX,
            temp.path().join("broker-stale.sock").to_string_lossy(),
            Utc::now(),
        )
        .unwrap();
        fs::write(&runtime_file, serde_json::to_vec(&stale).unwrap()).unwrap();
        fs::set_permissions(&runtime_file, fs::Permissions::from_mode(0o600)).unwrap();

        remove_stale_discovery(&runtime_file, Uuid::from_u128(2));
        assert!(runtime_file.exists());
        remove_stale_discovery(&runtime_file, stale.runtime_id());
        assert!(!runtime_file.exists());
    }
}
