//! Connection management: live sqlx pools (a separate read-only pool per
//! connection), OS credential-store secret storage, and per-provider connection-string
//! tuning. Long-lived local credentials live only in the OS credential store; managed
//! credentials are short-lived process-memory leases. MCP sees connection ids only.

pub mod keychain;
pub mod pool;
pub mod providers;
mod runtime;

pub use crate::driver::connect;
pub use keychain::{delete_secret, fetch_secret, store_secret};
pub use pool::{DbPool, LiveConnection};
pub(crate) use runtime::{
    ConnectionAccess, ConnectionContext, ConnectionLease, ConnectionManager,
    ConnectionOperationScope,
};

/// The executor module refers to the engine-tagged pool enum as `Pool`; keep a
/// single definition (`DbPool`) and expose this alias so both names resolve.
pub use pool::DbPool as Pool;

use crate::error::{AppError, AppResult};
use crate::model::{ConnectionProfile, WorkspaceConnectionAccess, WorkspaceCredentialMode};
use uuid::Uuid;

/// Resolve the credential item referenced by a profile. Shared templates must carry
/// an account-specific binding; they never fall back to the connection UUID where a
/// different signed-in account's legacy credential could exist.
pub fn fetch_profile_secret(profile: &ConnectionProfile) -> AppResult<String> {
    if profile.credential_mode == WorkspaceCredentialMode::Managed {
        return Err(AppError::Config(
            "managed credentials must be obtained from a short-lived lease".into(),
        ));
    }
    let secret_id = match profile.secret_ref.as_deref() {
        Some(secret_ref) => Uuid::parse_str(secret_ref)
            .map_err(|_| AppError::Config("connection secret reference is invalid".into()))?,
        // A local profile without a reference intentionally uses socket/trust/no
        // password authentication. A referenced-but-missing item still fails.
        None if profile.workspace_access == WorkspaceConnectionAccess::Local => {
            return Ok(String::new())
        }
        None => {
            return Err(AppError::NotFound(format!(
                "no credential binding for shared connection {}",
                profile.id
            )))
        }
    };
    fetch_secret(&secret_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(access: WorkspaceConnectionAccess) -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::new_v4(),
            name: "test".into(),
            engine: crate::model::Engine::Postgres,
            provider: crate::model::Provider::Generic,
            driver_id: None,
            host: "localhost".into(),
            port: 5432,
            database: "postgres".into(),
            username: "postgres".into(),
            sslmode: "prefer".into(),
            extra_params: Default::default(),
            readonly_default: true,
            allow_writes: false,
            secret_ref: None,
            env: None,
            schema_group: None,
            workspace_access: access,
            credential_mode: if access == WorkspaceConnectionAccess::Local {
                crate::model::WorkspaceCredentialMode::Local
            } else {
                crate::model::WorkspaceCredentialMode::MemberLocal
            },
        }
    }

    #[test]
    fn only_unreferenced_local_profiles_may_use_an_empty_secret() {
        assert_eq!(
            fetch_profile_secret(&profile(WorkspaceConnectionAccess::Local)).unwrap(),
            ""
        );
        assert!(matches!(
            fetch_profile_secret(&profile(WorkspaceConnectionAccess::Read)),
            Err(AppError::NotFound(_))
        ));
        let mut managed = profile(WorkspaceConnectionAccess::Read);
        managed.credential_mode = WorkspaceCredentialMode::Managed;
        assert!(matches!(
            fetch_profile_secret(&managed),
            Err(AppError::Config(_))
        ));
    }
}

/// One open connection of either family: the sqlx SQL stack or the MongoDB
/// document adapter. Callers pull this out of the shared connection map and
/// downcast with [`Live::sql`]/[`Live::mongo`] — a family mismatch is a hard,
/// fail-closed error, never a silent fallthrough.
#[derive(Clone)]
pub enum Live {
    Sql(LiveConnection),
    Mongo(crate::mongo::MongoConnection),
}

impl Live {
    /// The sqlx side of this connection; clear error for document databases.
    pub fn sql(&self) -> AppResult<&LiveConnection> {
        match self {
            Live::Sql(live) => Ok(live),
            Live::Mongo(_) => Err(AppError::Config(
                "this is a MongoDB document connection — SQL operations are not available on it"
                    .into(),
            )),
        }
    }

    /// The MongoDB side of this connection; clear error for SQL engines.
    pub fn mongo(&self) -> AppResult<&crate::mongo::MongoConnection> {
        match self {
            Live::Mongo(conn) => Ok(conn),
            Live::Sql(_) => Err(AppError::Config(
                "this is a SQL connection — document operations are not available on it".into(),
            )),
        }
    }

    /// Liveness probe against the live server.
    pub async fn test(&self) -> AppResult<()> {
        match self {
            Live::Sql(live) => live.test().await,
            Live::Mongo(conn) => conn.ping().await,
        }
    }

    /// Close provider-backed pools when their lease expires. Mongo clients have no
    /// asynchronous close primitive; dropping their final handle closes the client.
    pub async fn close(&self) {
        if let Live::Sql(live) = self {
            live.close().await;
        }
    }
}
