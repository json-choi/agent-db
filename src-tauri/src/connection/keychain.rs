//! Connection secrets in the OS credential store (macOS Keychain or Windows
//! Credential Manager through keyring 4).
//! Service = bundle id, account = connection id. The app.db holds only a
//! `secret_ref`; the password never touches disk in cleartext.
//! A zeroizing process-session cache avoids reopening the OS credential store for
//! every query or membership request. The OS store remains the at-rest authority.
//!
//! PRODUCTION REQUIRES A SIGNED BUILD. Unsigned / ad-hoc builds hit
//! platform credential-store failures (for example macOS `errSecMissingEntitlement
//! (-34018)`). So in DEBUG builds only we fall back to an obfuscated file under the
//! app data dir.
//! That fallback is NOT real security; it exists solely so unsigned dev builds run.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

use keyring::Entry;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};

/// Credential-store service name (bundle id). Must match the signed bundle identifier.
const SERVICE: &str = "capital.launcher.dopedb";
const LEGACY_WORKSPACE_SESSION_ACCOUNT: &str = "workspace-session";
static SESSION_CACHE: LazyLock<Mutex<HashMap<String, Zeroizing<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn session_cache() -> MutexGuard<'static, HashMap<String, Zeroizing<String>>> {
    SESSION_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cached_secret(account: &str) -> Option<String> {
    session_cache()
        .get(account)
        .map(|secret| secret.as_str().to_owned())
}

fn remember_secret(account: &str, secret: &str) {
    session_cache().insert(account.to_owned(), Zeroizing::new(secret.to_owned()));
}

fn read_cached_secret(
    account: &str,
    read: impl FnOnce() -> AppResult<String>,
) -> AppResult<String> {
    if let Some(secret) = cached_secret(account) {
        return Ok(secret);
    }
    let secret = read()?;
    remember_secret(account, &secret);
    Ok(secret)
}

fn forget_secret(account: &str) {
    session_cache().remove(account);
}

fn entry(account: &str) -> AppResult<Entry> {
    Ok(Entry::new(SERVICE, account)?)
}

/// Store (or replace) the secret for a connection.
pub fn store_secret(connection_id: &Uuid, secret: &str) -> AppResult<()> {
    let account = connection_id.to_string();
    match entry(&account)?.set_password(secret) {
        Ok(()) => Ok(()),
        Err(e) if should_fallback(&e) => file_store(&account, secret),
        Err(e) => Err(e.into()),
    }?;
    remember_secret(&account, secret);
    Ok(())
}

/// Fetch the secret for a connection.
pub fn fetch_secret(connection_id: &Uuid) -> AppResult<String> {
    let account = connection_id.to_string();
    read_cached_secret(&account, || match entry(&account)?.get_password() {
        Ok(s) => Ok(s),
        Err(keyring::Error::NoEntry) if cfg!(debug_assertions) => file_fetch(&account),
        Err(keyring::Error::NoEntry) => Err(AppError::NotFound(format!(
            "no secret for connection {connection_id}"
        ))),
        Err(e) if should_fallback(&e) => file_fetch(&account),
        Err(e) => Err(e.into()),
    })
}

/// Delete a connection's secret. Missing is not an error.
pub fn delete_secret(connection_id: &Uuid) -> AppResult<()> {
    let account = connection_id.to_string();
    forget_secret(&account);
    match entry(&account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            let _ = file_delete(&account);
            Ok(())
        }
        Err(e) if should_fallback(&e) => file_delete(&account),
        Err(e) => Err(e.into()),
    }
}

fn workspace_session_account(user_id: &str) -> AppResult<String> {
    let user_id = Uuid::parse_str(user_id)
        .map_err(|_| AppError::Config("workspace account id is invalid".into()))?;
    Ok(format!("workspace-session:{user_id}"))
}

fn store_workspace_session_account(account: &str, token: &str) -> AppResult<()> {
    match entry(account)?.set_password(token) {
        Ok(()) => Ok(()),
        Err(e) if should_fallback(&e) => file_store(account, token),
        Err(e) => Err(e.into()),
    }?;
    remember_secret(account, token);
    Ok(())
}

fn fetch_workspace_session_account(account: &str) -> AppResult<Option<String>> {
    if let Some(token) = cached_secret(account) {
        return Ok(Some(token));
    }
    let token = match entry(account)?.get_password() {
        Ok(token) => Some(token),
        Err(keyring::Error::NoEntry) if cfg!(debug_assertions) => match file_fetch(account) {
            Ok(token) => Some(token),
            Err(AppError::NotFound(_)) => None,
            Err(error) => return Err(error),
        },
        Err(keyring::Error::NoEntry) => None,
        Err(e) if should_fallback(&e) => match file_fetch(account) {
            Ok(token) => Some(token),
            Err(AppError::NotFound(_)) => None,
            Err(error) => return Err(error),
        },
        Err(e) => return Err(e.into()),
    };
    if let Some(token) = token.as_deref() {
        remember_secret(account, token);
    }
    Ok(token)
}

fn delete_workspace_session_account(account: &str) -> AppResult<()> {
    forget_secret(account);
    match entry(account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            let _ = file_delete(account);
            Ok(())
        }
        Err(e) if should_fallback(&e) => file_delete(account),
        Err(e) => Err(e.into()),
    }
}

/// Store one Better Auth Bearer session in an account-specific credential item.
pub fn store_workspace_session(user_id: &str, token: &str) -> AppResult<()> {
    store_workspace_session_account(&workspace_session_account(user_id)?, token)
}

/// Read one account's stored session. A missing item is normal signed-out state.
pub fn fetch_workspace_session(user_id: &str) -> AppResult<Option<String>> {
    fetch_workspace_session_account(&workspace_session_account(user_id)?)
}

/// Delete one local Better Auth session. Missing state is idempotently signed out.
pub fn delete_workspace_session(user_id: &str) -> AppResult<()> {
    delete_workspace_session_account(&workspace_session_account(user_id)?)
}

/// Upgrade helper for releases that stored exactly one session under a fixed account.
/// Callers validate and copy the token before removing this legacy item.
pub(crate) fn fetch_legacy_workspace_session() -> AppResult<Option<String>> {
    fetch_workspace_session_account(LEGACY_WORKSPACE_SESSION_ACCOUNT)
}

pub(crate) fn delete_legacy_workspace_session() -> AppResult<()> {
    delete_workspace_session_account(LEGACY_WORKSPACE_SESSION_ACCOUNT)
}

/// True when the OS credential store is structurally unavailable (for example an
/// unsigned dev build) AND we are in a debug build permitted to use the file fallback.
fn should_fallback(e: &keyring::Error) -> bool {
    cfg!(debug_assertions)
        && matches!(
            e,
            keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_)
        )
}

// ---------------------------------------------------------------------------
// DEBUG-ONLY file fallback. Obfuscated, NOT encrypted with a real key.
// ponytail: XOR-with-static-key obfuscation. Ceiling: not secure against anyone
// with file read access — it only stops a casual `cat`. Upgrade path is a signed
// build so the OS credential store works and this whole section is dead.
// ---------------------------------------------------------------------------

const OBFUSCATION_KEY: &[u8] = b"dopedb-dev-only-not-secure-v1";

fn fallback_dir() -> AppResult<std::path::PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| AppError::Config("no data dir".into()))?
        .join("dopedb")
        .join("dev-secrets");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn fallback_path_in(dir: &std::path::Path, account: &str) -> std::path::PathBuf {
    // Connection ids were historically stored as their UUID filename. Preserve that
    // debug-only layout, but encode namespaced session accounts because `:` is not a
    // valid filename character on Windows and separators must never escape `dir`.
    let filename =
        if Uuid::parse_str(account).is_ok() || account == LEGACY_WORKSPACE_SESSION_ACCOUNT {
            account.to_owned()
        } else {
            format!("account-{}", hex::encode(account.as_bytes()))
        };
    dir.join(format!("{filename}.secret"))
}

fn xor(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ OBFUSCATION_KEY[i % OBFUSCATION_KEY.len()])
        .collect()
}

fn file_store(account: &str, secret: &str) -> AppResult<()> {
    file_store_at(&fallback_dir()?, account, secret)
}

fn file_store_at(dir: &std::path::Path, account: &str, secret: &str) -> AppResult<()> {
    let obfuscated = hex::encode(xor(secret.as_bytes()));
    let path = fallback_path_in(dir, account);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        std::io::Write::write_all(&mut file, obfuscated.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, obfuscated)?;
    Ok(())
}

fn file_fetch(account: &str) -> AppResult<String> {
    file_fetch_at(&fallback_dir()?, account)
}

fn file_fetch_at(dir: &std::path::Path, account: &str) -> AppResult<String> {
    let path = fallback_path_in(dir, account);
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| AppError::NotFound(format!("no secret for account {account}")))?;
    let bytes = hex::decode(raw.trim())
        .map_err(|e| AppError::Config(format!("corrupt dev secret: {e}")))?;
    String::from_utf8(xor(&bytes)).map_err(|e| AppError::Config(format!("corrupt dev secret: {e}")))
}

fn file_delete(account: &str) -> AppResult<()> {
    file_delete_at(&fallback_dir()?, account)
}

fn file_delete_at(dir: &std::path::Path, account: &str) -> AppResult<()> {
    let path = fallback_path_in(dir, account);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_roundtrips() {
        let secret = "p@ssw0rd:with/special?chars";
        let obf = xor(secret.as_bytes());
        assert_ne!(obf, secret.as_bytes());
        assert_eq!(xor(&obf), secret.as_bytes());
    }

    /// Exercise the debug fallback on every CI platform without opening an
    /// interactive OS credential-store prompt.
    #[test]
    fn fallback_store_fetch_delete_roundtrip() {
        let id = Uuid::new_v4();
        let secret = "p@ss w0rd/üñî☃ non-ascii";
        let account = id.to_string();
        let dir = tempfile::tempdir().expect("tempdir");

        file_store_at(dir.path(), &account, secret).expect("store");
        assert_eq!(file_fetch_at(dir.path(), &account).expect("fetch"), secret);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(fallback_path_in(dir.path(), &account))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        file_delete_at(dir.path(), &account).expect("delete");
        assert!(
            file_fetch_at(dir.path(), &account).is_err(),
            "deleted secret must not fetch"
        );
        file_delete_at(dir.path(), &account).expect("delete is idempotent");
    }

    #[test]
    fn session_cache_reads_the_store_once_and_zeroizes_on_removal() {
        use std::cell::Cell;

        let account = format!("test-cache-{}", Uuid::new_v4());
        let reads = Cell::new(0);
        let first = read_cached_secret(&account, || {
            reads.set(reads.get() + 1);
            Ok("session-only-secret".to_owned())
        })
        .unwrap();
        let second = read_cached_secret(&account, || {
            reads.set(reads.get() + 1);
            Ok("must-not-be-read".to_owned())
        })
        .unwrap();

        assert_eq!(first, "session-only-secret");
        assert_eq!(second, "session-only-secret");
        assert_eq!(reads.get(), 1);
        forget_secret(&account);
        assert!(cached_secret(&account).is_none());
    }

    #[test]
    fn workspace_sessions_have_distinct_validated_credential_accounts() {
        let first = workspace_session_account("10000000-0000-0000-0000-000000000001").unwrap();
        let second = workspace_session_account("20000000-0000-0000-0000-000000000002").unwrap();

        assert_ne!(first, second);
        assert!(first.starts_with("workspace-session:"));
        assert!(workspace_session_account("../untrusted").is_err());
    }

    #[test]
    fn namespaced_fallback_accounts_use_portable_flat_filenames() {
        let dir = tempfile::tempdir().expect("tempdir");
        let account = workspace_session_account("10000000-0000-0000-0000-000000000001").unwrap();
        let path = fallback_path_in(dir.path(), &account);

        assert_eq!(path.parent(), Some(dir.path()));
        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(!filename.contains(':'));
        assert!(!filename.contains('/') && !filename.contains('\\'));
    }

    /// Real credential stores can require an interactive desktop session, which
    /// GitHub-hosted runners do not provide. Run this explicitly on a signed or
    /// otherwise credential-store-enabled desktop build.
    #[test]
    #[ignore = "requires an interactive OS credential store"]
    fn os_secret_store_fetch_delete_roundtrip() {
        let id = Uuid::new_v4();
        let secret = "p@ss w0rd/üñî☃ non-ascii";

        store_secret(&id, secret).expect("store");
        assert_eq!(fetch_secret(&id).expect("fetch"), secret);

        delete_secret(&id).expect("delete");
        assert!(fetch_secret(&id).is_err(), "deleted secret must not fetch");
        delete_secret(&id).expect("delete is idempotent");
    }
}
