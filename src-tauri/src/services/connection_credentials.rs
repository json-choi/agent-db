//! Narrow process-local boundary for long-lived database credentials.
//! Production delegates to the OS credential store; tests inject an in-memory vault.

use std::sync::Arc;

use uuid::Uuid;
use zeroize::Zeroizing;

use crate::connection;
use crate::error::AppResult;
use crate::model::ConnectionProfile;

pub(super) const MAX_CONNECTION_CREDENTIAL_BYTES: usize = 1 << 16;

pub(super) trait ConnectionCredentialVault: Send + Sync {
    fn fetch_profile(&self, profile: &ConnectionProfile) -> AppResult<Zeroizing<String>>;
    fn store(&self, id: &Uuid, secret: &str) -> AppResult<()>;
    fn delete(&self, id: &Uuid) -> AppResult<()>;
}

struct SystemConnectionCredentialVault;

impl ConnectionCredentialVault for SystemConnectionCredentialVault {
    fn fetch_profile(&self, profile: &ConnectionProfile) -> AppResult<Zeroizing<String>> {
        Ok(Zeroizing::new(connection::fetch_profile_secret(profile)?))
    }

    fn store(&self, id: &Uuid, secret: &str) -> AppResult<()> {
        connection::store_secret(id, secret)
    }

    fn delete(&self, id: &Uuid) -> AppResult<()> {
        connection::delete_secret(id)
    }
}

pub(super) fn system_connection_credentials() -> Arc<dyn ConnectionCredentialVault> {
    Arc::new(SystemConnectionCredentialVault)
}
