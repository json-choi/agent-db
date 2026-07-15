//! Connection secrets in the OS credential store (macOS Keychain or Windows
//! Credential Manager through keyring 3).
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

fn entry(connection_id: &Uuid) -> AppResult<Entry> {
    Ok(Entry::new(SERVICE, &connection_id.to_string())?)
}

/// Store (or replace) the secret for a connection.
pub fn store_secret(connection_id: &Uuid, secret: &str) -> AppResult<()> {
    match entry(connection_id)?.set_password(secret) {
        Ok(()) => Ok(()),
        Err(e) if should_fallback(&e) => file_store(connection_id, secret),
        Err(e) => Err(e.into()),
    }
}

/// Fetch the secret for a connection.
pub fn fetch_secret(connection_id: &Uuid) -> AppResult<String> {
    match entry(connection_id)?.get_password() {
        Ok(s) => Ok(s),
        Err(keyring::Error::NoEntry) if cfg!(debug_assertions) => file_fetch(connection_id),
        Err(keyring::Error::NoEntry) => Err(AppError::NotFound(format!(
            "no secret for connection {connection_id}"
        ))),
        Err(e) if should_fallback(&e) => file_fetch(connection_id),
        Err(e) => Err(e.into()),
    }
}

/// Delete a connection's secret. Missing is not an error.
pub fn delete_secret(connection_id: &Uuid) -> AppResult<()> {
    match entry(connection_id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            let _ = file_delete(connection_id);
            Ok(())
        }
        Err(e) if should_fallback(&e) => file_delete(connection_id),
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

fn fallback_path(connection_id: &Uuid) -> AppResult<std::path::PathBuf> {
    Ok(fallback_dir()?.join(format!("{connection_id}.secret")))
}

fn xor(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ OBFUSCATION_KEY[i % OBFUSCATION_KEY.len()])
        .collect()
}

fn file_store(connection_id: &Uuid, secret: &str) -> AppResult<()> {
    let obfuscated = hex::encode(xor(secret.as_bytes()));
    std::fs::write(fallback_path(connection_id)?, obfuscated)?;
    Ok(())
}

fn file_fetch(connection_id: &Uuid) -> AppResult<String> {
    let path = fallback_path(connection_id)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| AppError::NotFound(format!("no secret for connection {connection_id}")))?;
    let bytes = hex::decode(raw.trim())
        .map_err(|e| AppError::Config(format!("corrupt dev secret: {e}")))?;
    String::from_utf8(xor(&bytes)).map_err(|e| AppError::Config(format!("corrupt dev secret: {e}")))
}

fn file_delete(connection_id: &Uuid) -> AppResult<()> {
    let path = fallback_path(connection_id)?;
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

        file_store(&id, secret).expect("store");
        assert_eq!(file_fetch(&id).expect("fetch"), secret);

        file_delete(&id).expect("delete");
        assert!(file_fetch(&id).is_err(), "deleted secret must not fetch");
        file_delete(&id).expect("delete is idempotent");
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
