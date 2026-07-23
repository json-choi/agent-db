//! Connection application service and the deliberately narrow agent-facing summary DTO.
//! Full profiles remain available to the existing UI path; agent summaries are allowlisted.

use std::fmt;

use serde::Serialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::model::{ConnectionProfile, Engine};
use crate::store::Store;

/// A connection projection safe to serialize for an agent transport.
///
/// This allowlist intentionally has no provider, driver, network host/port, user,
/// credential reference, workspace/account authority, or provider-specific parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentConnectionSummary {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) engine: Engine,
    pub(crate) database: String,
    pub(crate) environment: Option<String>,
    pub(crate) readonly: bool,
    pub(crate) allow_writes: bool,
}

impl From<&ConnectionProfile> for AgentConnectionSummary {
    fn from(profile: &ConnectionProfile) -> Self {
        Self {
            id: profile.id,
            name: profile.name.clone(),
            engine: profile.engine,
            database: profile.database.clone(),
            environment: profile.env.clone(),
            readonly: profile.readonly_default,
            allow_writes: profile.allow_writes,
        }
    }
}

/// Domain-level legacy selector failures. Transport adapters map these variants to
/// their own error envelope without pulling rmcp or Tauri types into this service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LegacyConnectionResolutionError {
    NoConnections,
    NoMatch { selector: String },
}

impl fmt::Display for LegacyConnectionResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConnections => formatter.write_str("no connections are available"),
            Self::NoMatch { selector } => {
                write!(formatter, "no connection matches selector '{selector}'")
            }
        }
    }
}

impl std::error::Error for LegacyConnectionResolutionError {}

/// The outer [`AppResult`] of [`ConnectionService::resolve_legacy_mcp`] represents
/// store/authority failures; this inner result represents only selector semantics.
pub(crate) type LegacyConnectionResolution =
    Result<AgentConnectionSummary, LegacyConnectionResolutionError>;

#[derive(Clone)]
pub(crate) struct ConnectionService {
    store: Store,
}

impl ConnectionService {
    pub(super) fn new(store: Store) -> Self {
        Self { store }
    }

    /// Preserve the existing UI contract, including its full non-plaintext profile.
    pub(crate) async fn list_profiles(&self) -> AppResult<Vec<ConnectionProfile>> {
        self.store.list_connections().await
    }

    /// Return only the explicit agent allowlist; secret-bearing profile fields never
    /// become members of the serialized DTO.
    pub(crate) async fn list_agent_summaries(&self) -> AppResult<Vec<AgentConnectionSummary>> {
        Ok(self
            .list_profiles()
            .await?
            .iter()
            .map(AgentConnectionSummary::from)
            .collect())
    }

    /// Preserve the legacy MCP selector exactly: profiles are already name-ordered by
    /// [`Store::list_connections`]; `None` chooses the first, while `Some` finds the
    /// first exact UUID string or exact case-sensitive name.
    ///
    /// The outer result is an application/store failure. The inner result is a
    /// transport-neutral `NoConnections`/`NoMatch` selection outcome.
    pub(crate) async fn resolve_legacy_mcp(
        &self,
        selector: Option<&str>,
    ) -> AppResult<LegacyConnectionResolution> {
        let profiles = self.list_profiles().await?;
        Ok(resolve_legacy_profiles(&profiles, selector))
    }
}

fn resolve_legacy_profiles(
    profiles: &[ConnectionProfile],
    selector: Option<&str>,
) -> LegacyConnectionResolution {
    match selector {
        Some(selector) => profiles
            .iter()
            .find(|profile| profile.id.to_string() == selector || profile.name == selector)
            .map(AgentConnectionSummary::from)
            .ok_or_else(|| LegacyConnectionResolutionError::NoMatch {
                selector: selector.to_string(),
            }),
        None => profiles
            .first()
            .map(AgentConnectionSummary::from)
            .ok_or(LegacyConnectionResolutionError::NoConnections),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::model::{Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode};

    const ALPHA_ID: &str = "018f9999-8888-7777-8666-555544443331";
    const BETA_ID: &str = "018f9999-8888-7777-8666-555544443332";

    fn profile(id: &str, name: &str) -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::parse_str(id).unwrap(),
            name: name.into(),
            engine: Engine::Postgres,
            provider: Provider::Neon,
            driver_id: Some("secret-driver".into()),
            host: "secret-host.example".into(),
            port: 5432,
            database: "analytics".into(),
            username: "secret-user".into(),
            sslmode: "require".into(),
            extra_params: HashMap::from([("secret-param".into(), "secret-value".into())]),
            readonly_default: true,
            allow_writes: false,
            secret_ref: Some("secret-reference".into()),
            env: Some("prod".into()),
            schema_group: Some("secret-schema-group".into()),
            workspace_access: WorkspaceConnectionAccess::Manage,
            credential_mode: WorkspaceCredentialMode::Managed,
        }
    }

    fn profiles() -> Vec<ConnectionProfile> {
        vec![profile(ALPHA_ID, "alpha"), profile(BETA_ID, "beta")]
    }

    #[test]
    fn agent_summary_serializes_only_the_allowlisted_shape() {
        let value = serde_json::to_value(AgentConnectionSummary::from(&profiles()[0])).unwrap();

        assert_eq!(
            value,
            json!({
                "id": ALPHA_ID,
                "name": "alpha",
                "engine": "postgres",
                "database": "analytics",
                "environment": "prod",
                "readonly": true,
                "allowWrites": false,
            })
        );
        assert_eq!(value.as_object().map(serde_json::Map::len), Some(7));

        let serialized = value.to_string();
        for forbidden in [
            "secret-host",
            "secret-user",
            "secret-driver",
            "secret-param",
            "secret-value",
            "secret-reference",
            "secret-schema-group",
            "provider",
            "driverId",
            "host",
            "port",
            "username",
            "extraParams",
            "secretRef",
            "workspaceAccess",
            "credentialMode",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "agent summary leaked {forbidden}"
            );
        }
    }

    #[test]
    fn legacy_none_selects_the_first_name_ordered_profile() {
        let resolved = resolve_legacy_profiles(&profiles(), None).unwrap();
        assert_eq!(resolved.id, Uuid::parse_str(ALPHA_ID).unwrap());
    }

    #[test]
    fn legacy_exact_name_and_id_selectors_preserve_find_semantics() {
        let by_name = resolve_legacy_profiles(&profiles(), Some("beta")).unwrap();
        let by_id = resolve_legacy_profiles(&profiles(), Some(ALPHA_ID)).unwrap();

        assert_eq!(by_name.id, Uuid::parse_str(BETA_ID).unwrap());
        assert_eq!(by_id.name, "alpha");
        assert_eq!(
            resolve_legacy_profiles(&profiles(), Some("Beta")),
            Err(LegacyConnectionResolutionError::NoMatch {
                selector: "Beta".into(),
            })
        );
    }

    #[test]
    fn legacy_missing_selectors_distinguish_none_from_no_match() {
        assert_eq!(
            resolve_legacy_profiles(&[], None),
            Err(LegacyConnectionResolutionError::NoConnections)
        );
        assert_eq!(
            resolve_legacy_profiles(&[], Some("missing")),
            Err(LegacyConnectionResolutionError::NoMatch {
                selector: "missing".into(),
            })
        );
        assert_eq!(
            resolve_legacy_profiles(&profiles(), Some("missing")),
            Err(LegacyConnectionResolutionError::NoMatch {
                selector: "missing".into(),
            })
        );
    }
}
