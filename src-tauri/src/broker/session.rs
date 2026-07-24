//! In-memory Terminal session capabilities. Tokens never enter SQLite, discovery,
//! logs, argv, or serialized broker results.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dopedb_protocol::SessionAuthentication;
use subtle::ConstantTimeEq;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};
use crate::store::PinnedConnection;

const SESSION_TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BrokerCapability {
    ConnectionRead,
    ConnectionTest,
    CatalogRead,
    QueryPlan,
    QueryRun,
    SqlPropose,
    OperationRead,
    OperationCancel,
}

impl BrokerCapability {
    pub(crate) const ALL: [Self; 8] = [
        Self::ConnectionRead,
        Self::ConnectionTest,
        Self::CatalogRead,
        Self::QueryPlan,
        Self::QueryRun,
        Self::SqlPropose,
        Self::OperationRead,
        Self::OperationCancel,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthenticatedSession {
    pub(crate) terminal_session_id: Uuid,
    pub(crate) runtime_id: Uuid,
    pub(crate) workspace_id: Uuid,
    pub(crate) account_scope: String,
    pub(crate) connection_id: Uuid,
    pub(crate) connection_revision: i64,
    pub(crate) capabilities: BTreeSet<BrokerCapability>,
    pub(crate) expires_at: DateTime<Utc>,
}

impl AuthenticatedSession {
    pub(crate) fn require(&self, capability: BrokerCapability) -> AppResult<()> {
        if self.capabilities.contains(&capability) {
            Ok(())
        } else {
            Err(AppError::Blocked {
                reason: "terminal session does not have the required broker capability".into(),
            })
        }
    }
}

struct SessionRecord {
    metadata: AuthenticatedSession,
    token: Zeroizing<[u8; SESSION_TOKEN_BYTES]>,
}

impl fmt::Debug for SessionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRecord")
            .field("metadata", &self.metadata)
            .field("token", &"<redacted>")
            .finish()
    }
}

pub(crate) struct IssuedSessionCapability {
    pub(crate) terminal_session_id: Uuid,
    token: Zeroizing<String>,
    pub(crate) expires_at: DateTime<Utc>,
}

impl IssuedSessionCapability {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

impl fmt::Debug for IssuedSessionCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedSessionCapability")
            .field("terminal_session_id", &self.terminal_session_id)
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct BrokerSessionRegistry {
    runtime_id: Uuid,
    sessions: std::sync::Arc<DashMap<Uuid, SessionRecord>>,
}

impl BrokerSessionRegistry {
    pub(crate) fn new(runtime_id: Uuid) -> Self {
        Self {
            runtime_id,
            sessions: std::sync::Arc::new(DashMap::new()),
        }
    }

    pub(crate) fn issue(
        &self,
        terminal_session_id: Uuid,
        pin: &PinnedConnection,
        capabilities: impl IntoIterator<Item = BrokerCapability>,
        ttl: Duration,
    ) -> AppResult<IssuedSessionCapability> {
        if ttl.is_zero() {
            return Err(AppError::Config(
                "terminal session capability TTL must be positive".into(),
            ));
        }
        let mut token = Zeroizing::new([0u8; SESSION_TOKEN_BYTES]);
        getrandom::fill(token.as_mut()).map_err(|_| {
            AppError::Config("operating system random source is unavailable".into())
        })?;
        let expires_at = Utc::now()
            + chrono::Duration::from_std(ttl)
                .map_err(|_| AppError::Config("terminal session TTL is too large".into()))?;
        let metadata = AuthenticatedSession {
            terminal_session_id,
            runtime_id: self.runtime_id,
            workspace_id: pin.scope.workspace_id,
            account_scope: pin.scope.account_scope.storage_key().into(),
            connection_id: pin.connection_id,
            connection_revision: pin.connection_revision,
            capabilities: capabilities.into_iter().collect(),
            expires_at,
        };
        self.sessions.insert(
            terminal_session_id,
            SessionRecord {
                metadata,
                token: token.clone(),
            },
        );
        Ok(IssuedSessionCapability {
            terminal_session_id,
            token: Zeroizing::new(hex::encode(token.as_ref())),
            expires_at,
        })
    }

    pub(crate) fn authenticate(
        &self,
        authentication: &SessionAuthentication,
    ) -> AppResult<AuthenticatedSession> {
        let Some(record) = self.sessions.get(&authentication.terminal_session_id) else {
            return Err(authentication_denied());
        };
        if record.metadata.runtime_id != self.runtime_id || record.metadata.expires_at <= Utc::now()
        {
            drop(record);
            self.sessions.remove(&authentication.terminal_session_id);
            return Err(authentication_denied());
        }
        let mut supplied = Zeroizing::new([0u8; SESSION_TOKEN_BYTES]);
        if hex::decode_to_slice(authentication.token(), supplied.as_mut()).is_err()
            || !bool::from(record.token.as_ref().ct_eq(supplied.as_ref()))
        {
            return Err(authentication_denied());
        }
        Ok(record.metadata.clone())
    }

    pub(crate) fn revoke(&self, terminal_session_id: Uuid) -> bool {
        self.sessions.remove(&terminal_session_id).is_some()
    }

    pub(crate) fn revoke_connection(&self, connection_id: Uuid) -> usize {
        let ids = self
            .sessions
            .iter()
            .filter_map(|entry| {
                (entry.metadata.connection_id == connection_id).then_some(*entry.key())
            })
            .collect::<Vec<_>>();
        let count = ids.len();
        for id in ids {
            self.sessions.remove(&id);
        }
        count
    }

    pub(crate) fn revoke_all(&self) {
        self.sessions.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.sessions.len()
    }
}

fn authentication_denied() -> AppError {
    AppError::Blocked {
        reason: "terminal session authentication was denied".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use std::collections::HashMap;

    use crate::model::{
        ConnectionProfile, Engine, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode,
        WorkspaceKind,
    };
    use crate::store::{AccountScope, ActiveResourceScope, CatalogCachePolicy};

    use super::*;

    fn pin(connection_id: Uuid) -> PinnedConnection {
        PinnedConnection {
            scope: ActiveResourceScope {
                workspace_id: Uuid::nil(),
                workspace_kind: WorkspaceKind::Personal,
                selected_account_id: None,
                account_scope: AccountScope::Personal,
                generation: 7,
            },
            connection_id,
            connection_revision: 11,
            binding_revision: 3,
            binding_updated_at: Utc::now().to_rfc3339(),
            profile: ConnectionProfile {
                id: connection_id,
                name: "fixture".into(),
                engine: Engine::Sqlite,
                provider: Provider::Auto,
                driver_id: None,
                host: String::new(),
                port: 0,
                database: ":memory:".into(),
                username: String::new(),
                sslmode: "disable".into(),
                extra_params: HashMap::new(),
                readonly_default: true,
                allow_writes: false,
                secret_ref: None,
                env: Some("dev".into()),
                schema_group: None,
                workspace_access: WorkspaceConnectionAccess::Local,
                credential_mode: WorkspaceCredentialMode::Local,
            },
            requires_remote_rbac: false,
            catalog_cache_policy: CatalogCachePolicy::Persistent,
        }
    }

    #[test]
    fn capability_is_256_bit_redacted_and_memory_only() {
        let runtime_id = Uuid::new_v4();
        let registry = BrokerSessionRegistry::new(runtime_id);
        let session_id = Uuid::new_v4();
        let issued = registry
            .issue(
                session_id,
                &pin(Uuid::new_v4()),
                [BrokerCapability::QueryPlan],
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(issued.token().len(), SESSION_TOKEN_BYTES * 2);
        assert!(hex::decode(issued.token()).is_ok());
        let debug = format!("{issued:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(issued.token()));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn authentication_is_exact_scope_capability_and_revocation_bound() {
        let runtime_id = Uuid::from_str("018f0000-1111-7222-8333-444455556666").unwrap();
        let connection_id = Uuid::new_v4();
        let registry = BrokerSessionRegistry::new(runtime_id);
        let issued = registry
            .issue(
                Uuid::new_v4(),
                &pin(connection_id),
                [BrokerCapability::QueryPlan],
                Duration::from_secs(60),
            )
            .unwrap();
        let authentication =
            SessionAuthentication::new(issued.terminal_session_id, issued.token().to_owned());
        let authenticated = registry.authenticate(&authentication).unwrap();
        assert_eq!(authenticated.connection_id, connection_id);
        assert_eq!(authenticated.connection_revision, 11);
        assert!(authenticated.require(BrokerCapability::QueryPlan).is_ok());
        assert!(authenticated.require(BrokerCapability::SqlPropose).is_err());

        let wrong = SessionAuthentication::new(issued.terminal_session_id, "00".repeat(32));
        assert!(registry.authenticate(&wrong).is_err());
        assert!(registry.revoke(issued.terminal_session_id));
        assert!(registry.authenticate(&authentication).is_err());
    }

    #[test]
    fn connection_revocation_removes_only_matching_sessions() {
        let registry = BrokerSessionRegistry::new(Uuid::new_v4());
        let first_connection = Uuid::new_v4();
        let second_connection = Uuid::new_v4();
        registry
            .issue(
                Uuid::new_v4(),
                &pin(first_connection),
                [BrokerCapability::ConnectionRead],
                Duration::from_secs(60),
            )
            .unwrap();
        registry
            .issue(
                Uuid::new_v4(),
                &pin(second_connection),
                [BrokerCapability::ConnectionRead],
                Duration::from_secs(60),
            )
            .unwrap();
        assert_eq!(registry.revoke_connection(first_connection), 1);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn expired_capability_is_rejected_and_eagerly_removed() {
        let registry = BrokerSessionRegistry::new(Uuid::new_v4());
        let issued = registry
            .issue(
                Uuid::new_v4(),
                &pin(Uuid::new_v4()),
                [BrokerCapability::ConnectionRead],
                Duration::from_millis(1),
            )
            .unwrap();
        let authentication =
            SessionAuthentication::new(issued.terminal_session_id, issued.token().to_owned());
        std::thread::sleep(Duration::from_millis(5));
        assert!(registry.authenticate(&authentication).is_err());
        assert_eq!(registry.len(), 0);
    }
}
