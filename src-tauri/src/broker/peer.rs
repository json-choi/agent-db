//! OS peer identity and owner-only endpoint permissions.

#[cfg(unix)]
pub(crate) fn verify_unix_peer(stream: &tokio::net::UnixStream) -> std::io::Result<()> {
    let peer = stream.peer_cred()?;
    let current = unsafe { libc::geteuid() };
    if peer.uid() == current {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "broker peer belongs to a different OS user",
        ))
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::{c_void, OsStr};
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::ptr;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, OpenProcessToken, SetFileSecurityW, TokenUser,
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const OWNER_ONLY_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";

    pub(crate) fn create_named_pipe(
        endpoint: &str,
        first_instance: bool,
    ) -> io::Result<NamedPipeServer> {
        let descriptor = SecurityDescriptor::owner_only()?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.raw,
            bInheritHandle: 0,
        };
        let mut options = ServerOptions::new();
        options
            .access_inbound(true)
            .access_outbound(true)
            .first_pipe_instance(first_instance)
            .reject_remote_clients(true);
        unsafe {
            options.create_with_security_attributes_raw(
                endpoint,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
        }
    }

    pub(crate) fn verify_named_pipe_peer(stream: &NamedPipeServer) -> io::Result<()> {
        let pipe = stream.as_raw_handle() as HANDLE;
        let mut client_pid = 0u32;
        if unsafe { GetNamedPipeClientProcessId(pipe, &mut client_pid) } == 0 || client_pid == 0 {
            return Err(io::Error::last_os_error());
        }
        let client_process = OwnedHandle::new(unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, client_pid)
        })?;
        let client_token = open_process_token(client_process.raw())?;
        let current_token = open_process_token(unsafe { GetCurrentProcess() })?;
        let client_user = TokenUserBuffer::read(client_token.raw())?;
        let current_user = TokenUserBuffer::read(current_token.raw())?;
        if unsafe { EqualSid(client_user.sid(), current_user.sid()) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "broker peer belongs to a different OS user",
            ));
        }
        Ok(())
    }

    pub(crate) fn restrict_path_to_current_user(path: &Path) -> crate::error::AppResult<()> {
        let descriptor = SecurityDescriptor::owner_only()?;
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let flags = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        if unsafe { SetFileSecurityW(wide.as_ptr(), flags, descriptor.raw) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    struct SecurityDescriptor {
        raw: PSECURITY_DESCRIPTOR,
    }

    impl SecurityDescriptor {
        fn owner_only() -> io::Result<Self> {
            let sddl = OsStr::new(OWNER_ONLY_SDDL)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            let mut raw = ptr::null_mut();
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut raw,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { raw })
        }
    }

    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            if !self.raw.is_null() {
                unsafe {
                    LocalFree(self.raw as HLOCAL);
                }
            }
        }
    }

    struct OwnedHandle(HANDLE);

    impl OwnedHandle {
        fn new(raw: HANDLE) -> io::Result<Self> {
            if raw.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(raw))
            }
        }

        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn open_process_token(process: HANDLE) -> io::Result<OwnedHandle> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedHandle::new(token)
    }

    struct TokenUserBuffer {
        words: Vec<usize>,
    }

    impl TokenUserBuffer {
        fn read(token: HANDLE) -> io::Result<Self> {
            let mut required = 0u32;
            unsafe {
                GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
            }
            if required < size_of::<TOKEN_USER>() as u32 {
                return Err(io::Error::last_os_error());
            }
            let word_bytes = size_of::<usize>();
            let words = usize::try_from(required)
                .ok()
                .and_then(|bytes| bytes.checked_add(word_bytes - 1))
                .map(|bytes| bytes / word_bytes)
                .ok_or_else(|| io::Error::other("token user buffer is too large"))?;
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
                return Err(io::Error::last_os_error());
            }
            Ok(buffer)
        }

        fn sid(&self) -> windows_sys::Win32::Security::PSID {
            let user = unsafe { &*(self.words.as_ptr().cast::<TOKEN_USER>()) };
            user.User.Sid
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::{
    create_named_pipe, restrict_path_to_current_user, verify_named_pipe_peer,
};

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unix_socket_pair_accepts_the_current_user() {
        let (first, second) = tokio::net::UnixStream::pair().unwrap();
        verify_unix_peer(&first).unwrap();
        verify_unix_peer(&second).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use tokio::net::windows::named_pipe::ClientOptions;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn owner_local_named_pipe_accepts_the_current_process() {
        let endpoint = format!(r"\\.\pipe\dopedb-peer-test-{}", Uuid::new_v4());
        let mut server = create_named_pipe(&endpoint, true).unwrap();
        let client = ClientOptions::new().open(&endpoint).unwrap();
        server.connect().await.unwrap();
        verify_named_pipe_peer(&server).unwrap();
        drop(client);
    }
}
