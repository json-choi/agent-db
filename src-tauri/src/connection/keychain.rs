//! Connection secrets in the OS credential store (macOS Keychain or Windows
//! Credential Manager through keyring 4).
//! Service = bundle id, account = connection id. The app.db holds only a
//! `secret_ref`; the password never touches disk in cleartext.
//!
//! PRODUCTION REQUIRES A SIGNED BUILD. Unsigned / ad-hoc builds hit
//! platform credential-store failures (for example macOS `errSecMissingEntitlement
//! (-34018)`). So in DEBUG builds only we fall back to an obfuscated file under the
//! app data dir.
//! That fallback is NOT real security; it exists solely so unsigned dev builds run.

use keyring::Entry;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Credential-store service name (bundle id). Must match the signed bundle identifier.
const SERVICE: &str = "capital.launcher.dopedb";
const WORKSPACE_SESSION_ACCOUNT: &str = "workspace-session";

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
    }
}

/// Fetch the secret for a connection.
pub fn fetch_secret(connection_id: &Uuid) -> AppResult<String> {
    let account = connection_id.to_string();
    match entry(&account)?.get_password() {
        Ok(s) => Ok(s),
        Err(keyring::Error::NoEntry) if cfg!(debug_assertions) => file_fetch(&account),
        Err(keyring::Error::NoEntry) => Err(AppError::NotFound(format!(
            "no secret for connection {connection_id}"
        ))),
        Err(e) if should_fallback(&e) => file_fetch(&account),
        Err(e) => Err(e.into()),
    }
}

/// Delete a connection's secret. Missing is not an error.
pub fn delete_secret(connection_id: &Uuid) -> AppResult<()> {
    let account = connection_id.to_string();
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

/// Store a Better Auth Bearer session without exposing it to the webview.
pub fn store_workspace_session(token: &str) -> AppResult<()> {
    match entry(WORKSPACE_SESSION_ACCOUNT)?.set_password(token) {
        Ok(()) => Ok(()),
        Err(e) if should_fallback(&e) => file_store(WORKSPACE_SESSION_ACCOUNT, token),
        Err(e) => Err(e.into()),
    }
}

/// Read the stored workspace session. A missing session is normal signed-out state.
pub fn fetch_workspace_session() -> AppResult<Option<String>> {
    match entry(WORKSPACE_SESSION_ACCOUNT)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) if cfg!(debug_assertions) => {
            match file_fetch(WORKSPACE_SESSION_ACCOUNT) {
                Ok(token) => Ok(Some(token)),
                Err(AppError::NotFound(_)) => Ok(None),
                Err(error) => Err(error),
            }
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) if should_fallback(&e) => match file_fetch(WORKSPACE_SESSION_ACCOUNT) {
            Ok(token) => Ok(Some(token)),
            Err(AppError::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        },
        Err(e) => Err(e.into()),
    }
}

/// Delete the local Better Auth session. Missing state is idempotently signed out.
pub fn delete_workspace_session() -> AppResult<()> {
    match entry(WORKSPACE_SESSION_ACCOUNT)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            let _ = file_delete(WORKSPACE_SESSION_ACCOUNT);
            Ok(())
        }
        Err(e) if should_fallback(&e) => file_delete(WORKSPACE_SESSION_ACCOUNT),
        Err(e) => Err(e.into()),
    }
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
    Ok(dir)
}

fn fallback_path_in(dir: &std::path::Path, account: &str) -> std::path::PathBuf {
    dir.join(format!("{account}.secret"))
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
    std::fs::write(fallback_path_in(dir, account), obfuscated)?;
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

        file_delete_at(dir.path(), &account).expect("delete");
        assert!(
            file_fetch_at(dir.path(), &account).is_err(),
            "deleted secret must not fetch"
        );
        file_delete_at(dir.path(), &account).expect("delete is idempotent");
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
