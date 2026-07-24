//! Bundled and global CLI resolution.
//!
//! The in-app Terminal always resolves the immutable sidecar shipped with this app.
//! A global install is a separate, explicit user action that copies that sidecar to
//! the per-user bin directory and, when requested, adds a small managed PATH block.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
#[cfg(not(windows))]
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

#[cfg(not(windows))]
const MANAGED_PATH_BEGIN: &str = "# >>> DopeDB CLI >>>";
#[cfg(not(windows))]
const MANAGED_PATH_END: &str = "# <<< DopeDB CLI <<<";
#[cfg(not(windows))]
const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedPathBlockState {
    Missing,
    Current,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallationStatus {
    pub version: String,
    pub bundled_available: bool,
    pub bundled_path: Option<String>,
    pub in_app_directory: Option<String>,
    pub install_path: String,
    pub installed: bool,
    pub current: bool,
    pub conflict: bool,
    pub path_configured: bool,
    pub path_change_required: bool,
    pub path_change_supported: bool,
    pub path_change_preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallReceipt {
    pub status: CliInstallationStatus,
    pub binary_changed: bool,
    pub path_changed: bool,
}

pub(crate) fn bundled_cli_binary() -> AppResult<PathBuf> {
    bundled_cli_candidates()?
        .into_iter()
        .find(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .ok_or_else(|| AppError::NotFound("the bundled DopeDB CLI sidecar".into()))
}

/// Directory prepended to every in-app Terminal PATH. Tauri keeps the bundled
/// sidecar name (`dopedb-cli`) distinct from the GUI executable; this resolver
/// atomically prepares an app-managed `dopedb` command from those exact signed
/// bytes. It never searches or reuses the mutable global installation.
pub(crate) fn in_app_cli_directory() -> AppResult<PathBuf> {
    let source = bundled_cli_binary()?;
    let target = in_app_cli_target()?;
    let current = fs::symlink_metadata(&target)
        .is_ok_and(|metadata| metadata.file_type().is_file())
        && files_equal(&source, &target)?;
    if !current {
        install_binary(&source, &target)?;
    }
    let directory = target
        .parent()
        .ok_or_else(|| AppError::Config("the in-app CLI target has no parent directory".into()))?
        .to_path_buf();
    restrict_in_app_cli_directory(&directory)?;
    Ok(directory)
}

pub(crate) fn installation_status() -> AppResult<CliInstallationStatus> {
    let source = bundled_cli_binary().ok();
    let in_app_directory = in_app_cli_directory().ok();
    let target = global_cli_target()?;
    status_for(source.as_deref(), in_app_directory.as_deref(), &target)
}

pub(crate) fn install(update_path: bool, replace_existing: bool) -> AppResult<CliInstallReceipt> {
    let source = bundled_cli_binary()?;
    let in_app_directory = in_app_cli_directory()?;
    let target = global_cli_target()?;
    let initial = status_for(Some(&source), Some(&in_app_directory), &target)?;
    if initial.conflict && !replace_existing {
        return Err(AppError::Blocked {
            reason: format!(
                "{} already exists and is not the bundled DopeDB CLI; confirm replacement explicitly",
                target.display()
            ),
        });
    }

    let binary_changed = !initial.current;
    if binary_changed {
        install_binary(&source, &target)?;
    }

    let mut path_changed = false;
    if update_path && initial.path_change_required {
        path_changed = configure_user_path(target.parent().ok_or_else(|| {
            AppError::Config("the global CLI target has no parent directory".into())
        })?)?;
    }

    let status = status_for(Some(&source), Some(&in_app_directory), &target)?;
    Ok(CliInstallReceipt {
        status,
        binary_changed,
        path_changed,
    })
}

fn bundled_cli_candidates() -> AppResult<Vec<PathBuf>> {
    let executable = std::env::current_exe()?;
    let executable_dir = executable
        .parent()
        .ok_or_else(|| AppError::Config("the app executable has no parent directory".into()))?;
    let binary_name = if cfg!(windows) {
        "dopedb-cli.exe"
    } else {
        "dopedb-cli"
    };
    let mut candidates = vec![
        executable_dir.join(binary_name),
        executable_dir.join("resources").join(binary_name),
    ];
    if executable_dir
        .file_name()
        .is_some_and(|component| component == "MacOS")
    {
        if let Some(contents) = executable_dir.parent() {
            candidates.push(contents.join("Resources").join(binary_name));
        }
    }

    if let Some(triple) = host_target_triple() {
        let extension = if cfg!(windows) { ".exe" } else { "" };
        candidates.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("binaries")
                .join(format!("dopedb-cli-{triple}{extension}")),
        );
    }
    candidates.dedup();
    Ok(candidates)
}

fn host_target_triple() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("aarch64", "windows") => Some("aarch64-pc-windows-msvc"),
        ("x86_64", "windows") => Some("x86_64-pc-windows-msvc"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

#[cfg(windows)]
fn global_cli_target() -> AppResult<PathBuf> {
    let local = dirs::data_local_dir()
        .ok_or_else(|| AppError::Config("no local application-data directory".into()))?;
    Ok(local.join("DopeDB").join("bin").join("dopedb.exe"))
}

#[cfg(not(windows))]
fn global_cli_target() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::Config("no home directory".into()))?;
    Ok(home.join(".local").join("bin").join("dopedb"))
}

fn in_app_cli_target() -> AppResult<PathBuf> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| AppError::Config("no local application-data directory".into()))?;
    Ok(base
        .join("dopedb")
        .join("cli")
        .join("bin")
        .join(if cfg!(windows) {
            "dopedb.exe"
        } else {
            "dopedb"
        }))
}

fn status_for(
    source: Option<&Path>,
    in_app_directory: Option<&Path>,
    target: &Path,
) -> AppResult<CliInstallationStatus> {
    let target_metadata = fs::symlink_metadata(target).ok();
    let installed = target_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.file_type().is_file());
    let current = match (source, installed) {
        (Some(source), true) => files_equal(source, target)?,
        _ => false,
    };
    let conflict = target_metadata.is_some() && !current;
    let parent = target
        .parent()
        .ok_or_else(|| AppError::Config("the global CLI target has no parent directory".into()))?;
    let path_configured = path_contains(parent) || managed_path_is_present()?;
    let path_change_supported = path_change_supported();
    let path_change_required = !path_configured;
    Ok(CliInstallationStatus {
        version: env!("CARGO_PKG_VERSION").into(),
        bundled_available: source.is_some(),
        bundled_path: source.map(|path| path.to_string_lossy().into_owned()),
        in_app_directory: in_app_directory.map(|path| path.to_string_lossy().into_owned()),
        install_path: target.to_string_lossy().into_owned(),
        installed,
        current,
        conflict,
        path_configured,
        path_change_required,
        path_change_supported,
        path_change_preview: path_change_required.then(|| path_change_preview(parent)),
    })
}

fn path_contains(directory: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|entry| same_path(&entry, directory)))
        .unwrap_or(false)
}

#[cfg(windows)]
fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn same_path(left: &Path, right: &Path) -> bool {
    left == right
}

fn files_equal(left: &Path, right: &Path) -> AppResult<bool> {
    let left_metadata = fs::symlink_metadata(left)?;
    let right_metadata = fs::symlink_metadata(right)?;
    if !left_metadata.file_type().is_file()
        || !right_metadata.file_type().is_file()
        || left_metadata.len() != right_metadata.len()
    {
        return Ok(false);
    }
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    loop {
        let left_count = left.read(&mut left_buffer)?;
        let right_count = right.read(&mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}

fn install_binary(source: &Path, target: &Path) -> AppResult<()> {
    let source_metadata = fs::symlink_metadata(source)?;
    if !source_metadata.file_type().is_file() {
        return Err(AppError::Config(
            "the bundled CLI is not a regular file".into(),
        ));
    }
    let directory = target
        .parent()
        .ok_or_else(|| AppError::Config("the global CLI target has no parent directory".into()))?;
    fs::create_dir_all(directory)?;
    restrict_install_directory(directory)?;

    if fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(AppError::Blocked {
            reason: format!("refusing to replace symbolic link {}", target.display()),
        });
    }

    let temporary = directory.join(format!(".dopedb-cli-{}.tmp", Uuid::new_v4()));
    let result = (|| -> AppResult<()> {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        make_executable(&temporary)?;
        atomic_replace(&temporary, target)?;
        if let Ok(directory_file) = OpenOptions::new().read(true).open(directory) {
            let _ = directory_file.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn restrict_install_directory(directory: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(windows)]
fn restrict_install_directory(_directory: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_in_app_cli_directory(directory: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(windows)]
fn restrict_in_app_cli_directory(_directory: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(windows)]
fn make_executable(_path: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
fn atomic_replace(from: &Path, to: &Path) -> AppResult<()> {
    fs::rename(from, to)?;
    Ok(())
}

#[cfg(windows)]
fn atomic_replace(from: &Path, to: &Path) -> AppResult<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let from = wide_null(from.as_os_str());
    let to = wide_null(to.as_os_str());
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(windows)]
fn wide_null(value: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(not(windows))]
fn path_change_supported() -> bool {
    true
}

#[cfg(windows)]
fn path_change_supported() -> bool {
    true
}

#[cfg(not(windows))]
fn shell_profile() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::Config("no home directory".into()))?;
    let shell = std::env::var_os("SHELL")
        .and_then(|value| PathBuf::from(value).file_name().map(OsStr::to_owned))
        .and_then(|value| value.into_string().ok())
        .unwrap_or_default();
    let name = match shell.as_str() {
        "zsh" => ".zprofile",
        "fish" => ".config/fish/conf.d/dopedb.fish",
        "bash" => ".bash_profile",
        _ => ".profile",
    };
    Ok(home.join(name))
}

#[cfg(not(windows))]
fn managed_path_is_present() -> AppResult<bool> {
    let profile = shell_profile()?;
    let Some(metadata) = fs::symlink_metadata(&profile).ok() else {
        return Ok(false);
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_PROFILE_BYTES {
        return Ok(false);
    }
    Ok(fs::read_to_string(profile).is_ok_and(|contents| {
        managed_path_block_state(&contents) == ManagedPathBlockState::Current
    }))
}

#[cfg(windows)]
fn managed_path_is_present() -> AppResult<bool> {
    windows_user_path().map(|value| {
        global_cli_target()
            .ok()
            .and_then(|target| target.parent().map(Path::to_path_buf))
            .is_some_and(|directory| {
                value
                    .split(';')
                    .filter(|entry| !entry.is_empty())
                    .any(|entry| same_path(Path::new(entry), &directory))
            })
    })
}

#[cfg(not(windows))]
fn path_change_preview(_directory: &Path) -> String {
    let profile = shell_profile()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "~/.profile".into());
    format!("{profile}\n\n{}", unix_path_block())
}

#[cfg(windows)]
fn path_change_preview(directory: &Path) -> String {
    format!("User PATH\n\nAppend: {}", directory.to_string_lossy())
}

#[cfg(not(windows))]
fn configure_user_path(_directory: &Path) -> AppResult<bool> {
    let profile = shell_profile()?;
    if let Some(parent) = profile.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(&profile).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(AppError::Blocked {
            reason: format!("refusing to edit symbolic link {}", profile.display()),
        });
    }
    let existing = match fs::metadata(&profile) {
        Ok(metadata) if metadata.len() <= MAX_PROFILE_BYTES => fs::read_to_string(&profile)
            .map_err(|_| AppError::Config("the shell profile is not valid UTF-8".into()))?,
        Ok(_) => {
            return Err(AppError::Blocked {
                reason: format!("shell profile {} is unexpectedly large", profile.display()),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    match managed_path_block_state(&existing) {
        ManagedPathBlockState::Current => return Ok(false),
        ManagedPathBlockState::Modified => {
            return Err(AppError::Blocked {
            reason: format!(
                "the managed DopeDB PATH block in {} was modified; preserve it and repair it manually",
                profile.display()
            ),
            })
        }
        ManagedPathBlockState::Missing => {}
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&profile)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(b"\n")?;
    file.write_all(unix_path_block().as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(true)
}

#[cfg(not(windows))]
fn unix_path_block() -> String {
    let body = if std::env::var_os("SHELL")
        .and_then(|value| PathBuf::from(value).file_name().map(OsStr::to_owned))
        .is_some_and(|value| value == "fish")
    {
        "fish_add_path --prepend --global $HOME/.local/bin"
    } else {
        "case \":$PATH:\" in\n  *\":$HOME/.local/bin:\"*) ;;\n  *) export PATH=\"$HOME/.local/bin:$PATH\" ;;\nesac"
    };
    format!("{MANAGED_PATH_BEGIN}\n{body}\n{MANAGED_PATH_END}")
}

#[cfg(not(windows))]
fn managed_path_block_state(contents: &str) -> ManagedPathBlockState {
    let block = unix_path_block();
    let begin_count = contents.matches(MANAGED_PATH_BEGIN).count();
    let end_count = contents.matches(MANAGED_PATH_END).count();
    if begin_count == 0 && end_count == 0 {
        ManagedPathBlockState::Missing
    } else if begin_count == 1 && end_count == 1 && contents.matches(&block).count() == 1 {
        ManagedPathBlockState::Current
    } else {
        ManagedPathBlockState::Modified
    }
}

#[cfg(windows)]
fn configure_user_path(directory: &Path) -> AppResult<bool> {
    let current = windows_user_path()?;
    if current
        .split(';')
        .filter(|entry| !entry.is_empty())
        .any(|entry| same_path(Path::new(entry), directory))
    {
        return Ok(false);
    }
    let mut updated = current.trim_end_matches(';').to_string();
    if !updated.is_empty() {
        updated.push(';');
    }
    updated.push_str(&directory.to_string_lossy());
    if updated.encode_utf16().count() >= 32_767 {
        return Err(AppError::Blocked {
            reason: "the Windows user PATH is too large to update safely".into(),
        });
    }
    set_windows_user_path(&updated)?;
    Ok(true)
}

#[cfg(windows)]
fn windows_user_path() -> AppResult<String> {
    windows_registry::read_user_path()
}

#[cfg(windows)]
fn set_windows_user_path(value: &str) -> AppResult<()> {
    windows_registry::write_user_path(value)
}

#[cfg(windows)]
mod windows_registry {
    use std::ffi::OsStr;
    use std::ptr;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    use super::{wide_null, AppError, AppResult};

    const ENVIRONMENT_KEY: &str = "Environment";
    const PATH_VALUE: &str = "Path";

    pub(super) fn read_user_path() -> AppResult<String> {
        let key = open_environment(KEY_QUERY_VALUE)?;
        let value_name = wide_null(OsStr::new(PATH_VALUE));
        let mut value_type = 0u32;
        let mut byte_count = 0u32;
        let first = unsafe {
            RegQueryValueExW(
                key.0,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut value_type,
                ptr::null_mut(),
                &mut byte_count,
            )
        };
        if first == ERROR_FILE_NOT_FOUND {
            return Ok(String::new());
        }
        if first != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(first as i32).into());
        }
        if value_type != REG_SZ && value_type != REG_EXPAND_SZ {
            return Err(AppError::Config(
                "the Windows user PATH has an unsupported registry type".into(),
            ));
        }
        if byte_count > 128 * 1024 {
            return Err(AppError::Blocked {
                reason: "the Windows user PATH is unexpectedly large".into(),
            });
        }
        let word_count = usize::try_from(byte_count)
            .ok()
            .and_then(|bytes| bytes.checked_add(1))
            .map(|bytes| bytes / 2)
            .ok_or_else(|| AppError::Config("the Windows user PATH is too large".into()))?;
        let mut buffer = vec![0u16; word_count.max(1)];
        let second = unsafe {
            RegQueryValueExW(
                key.0,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut value_type,
                buffer.as_mut_ptr().cast::<u8>(),
                &mut byte_count,
            )
        };
        if second != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(second as i32).into());
        }
        let end = buffer
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(buffer.len());
        Ok(String::from_utf16_lossy(&buffer[..end]))
    }

    pub(super) fn write_user_path(value: &str) -> AppResult<()> {
        let key = open_environment(KEY_SET_VALUE)?;
        let value_name = wide_null(OsStr::new(PATH_VALUE));
        let data = wide_null(OsStr::new(value));
        let byte_count = u32::try_from(data.len() * 2)
            .map_err(|_| AppError::Config("the Windows user PATH is too large".into()))?;
        let status = unsafe {
            RegSetValueExW(
                key.0,
                value_name.as_ptr(),
                0,
                REG_EXPAND_SZ,
                data.as_ptr().cast::<u8>(),
                byte_count,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32).into());
        }
        let environment = wide_null(OsStr::new("Environment"));
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                environment.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5_000,
                ptr::null_mut(),
            );
        }
        Ok(())
    }

    fn open_environment(access: u32) -> AppResult<RegistryKey> {
        let subkey = wide_null(OsStr::new(ENVIRONMENT_KEY));
        let mut key = ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, access, &mut key) };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32).into());
        }
        Ok(RegistryKey(key))
    }

    struct RegistryKey(HKEY);

    impl Drop for RegistryKey {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_app_resolver_never_searches_the_mutable_global_path() {
        let candidates = bundled_cli_candidates().unwrap();
        let target = global_cli_target().unwrap();
        assert!(!candidates.contains(&target));
        let in_app_target = in_app_cli_target().unwrap();
        assert_ne!(in_app_target, target);
        assert_eq!(
            in_app_target.file_name().unwrap(),
            if cfg!(windows) {
                "dopedb.exe"
            } else {
                "dopedb"
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_is_atomic_executable_and_detects_foreign_conflicts() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("bin").join("dopedb");
        fs::write(&source, b"fixture-cli").unwrap();
        install_binary(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"fixture-cli");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(files_equal(&source, &target).unwrap());

        fs::write(&target, b"foreign-cli").unwrap();
        let status = status_for(Some(&source), source.parent(), &target).unwrap();
        assert!(status.conflict);
        assert!(!status.current);
    }

    #[cfg(unix)]
    #[test]
    fn installer_refuses_to_follow_or_replace_a_target_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let foreign = directory.path().join("foreign");
        let target = directory.path().join("bin").join("dopedb");
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::write(&source, b"fixture-cli").unwrap();
        fs::write(&foreign, b"foreign-cli").unwrap();
        std::os::unix::fs::symlink(&foreign, &target).unwrap();

        assert!(matches!(
            install_binary(&source, &target),
            Err(AppError::Blocked { .. })
        ));
        assert_eq!(fs::read(foreign).unwrap(), b"foreign-cli");
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_is_atomic_and_replaces_only_the_exact_target() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.exe");
        let target = directory.path().join("bin").join("dopedb.exe");
        fs::write(&source, b"fixture-cli-v1").unwrap();
        install_binary(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"fixture-cli-v1");
        fs::write(&source, b"fixture-cli-v2").unwrap();
        install_binary(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"fixture-cli-v2");
    }

    #[cfg(not(windows))]
    #[test]
    fn managed_path_block_is_idempotent_and_does_not_embed_a_secret() {
        let block = unix_path_block();
        assert_eq!(block.matches(MANAGED_PATH_BEGIN).count(), 1);
        assert_eq!(block.matches(MANAGED_PATH_END).count(), 1);
        assert!(block.contains("$HOME/.local/bin"));
        for forbidden in ["token", "password", "credential"] {
            assert!(!block.to_ascii_lowercase().contains(forbidden));
        }
        assert_eq!(
            managed_path_block_state(&block),
            ManagedPathBlockState::Current
        );
        assert_eq!(
            managed_path_block_state(MANAGED_PATH_BEGIN),
            ManagedPathBlockState::Modified
        );
        assert_eq!(
            managed_path_block_state(&format!("{block}\n{MANAGED_PATH_BEGIN}")),
            ManagedPathBlockState::Modified
        );
        assert_eq!(
            managed_path_block_state(&block.replace("$HOME/.local/bin", "/tmp/foreign-bin")),
            ManagedPathBlockState::Modified
        );
        assert_eq!(
            managed_path_block_state("export PATH=/usr/bin"),
            ManagedPathBlockState::Missing
        );
    }
}
