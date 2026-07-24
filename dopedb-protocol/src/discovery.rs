//! Public, secret-free runtime discovery metadata.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const RUNTIME_SCHEMA_VERSION: u16 = 1;
pub const RUNTIME_DIRECTORY_NAME: &str = "runtime";
pub const RUNTIME_FILE_NAME: &str = "runtime.json";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeDiscovery {
    schema_version: u16,
    runtime_id: Uuid,
    pid: u32,
    app_version: String,
    protocol_min: u16,
    protocol_max: u16,
    endpoint: String,
    started_at: DateTime<Utc>,
}

impl RuntimeDiscovery {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime_id: Uuid,
        pid: u32,
        app_version: impl Into<String>,
        protocol_min: u16,
        protocol_max: u16,
        endpoint: impl Into<String>,
        started_at: DateTime<Utc>,
    ) -> Result<Self, RuntimeDiscoveryError> {
        let discovery = Self {
            schema_version: RUNTIME_SCHEMA_VERSION,
            runtime_id,
            pid,
            app_version: app_version.into(),
            protocol_min,
            protocol_max,
            endpoint: endpoint.into(),
            started_at,
        };
        discovery.validate()?;
        Ok(discovery)
    }

    pub fn validate(&self) -> Result<(), RuntimeDiscoveryError> {
        if self.schema_version != RUNTIME_SCHEMA_VERSION {
            return Err(RuntimeDiscoveryError::SchemaVersion);
        }
        if self.pid == 0 {
            return Err(RuntimeDiscoveryError::Pid);
        }
        if self.app_version.trim().is_empty() {
            return Err(RuntimeDiscoveryError::AppVersion);
        }
        if self.protocol_min == 0 || self.protocol_min > self.protocol_max {
            return Err(RuntimeDiscoveryError::ProtocolRange);
        }
        if self.endpoint.trim().is_empty() {
            return Err(RuntimeDiscoveryError::Endpoint);
        }
        Ok(())
    }

    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub const fn runtime_id(&self) -> Uuid {
        self.runtime_id
    }

    pub const fn pid(&self) -> u32 {
        self.pid
    }

    pub fn app_version(&self) -> &str {
        &self.app_version
    }

    pub const fn protocol_min(&self) -> u16 {
        self.protocol_min
    }

    pub const fn protocol_max(&self) -> u16 {
        self.protocol_max
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
}

impl fmt::Debug for RuntimeDiscovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeDiscovery")
            .field("schema_version", &self.schema_version)
            .field("runtime_id", &self.runtime_id)
            .field("pid", &self.pid)
            .field("app_version", &self.app_version)
            .field("protocol_min", &self.protocol_min)
            .field("protocol_max", &self.protocol_max)
            .field("endpoint", &"<redacted-local-endpoint>")
            .field("started_at", &self.started_at)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RuntimeDiscoveryError {
    #[error("unsupported runtime discovery schema")]
    SchemaVersion,
    #[error("runtime process id is invalid")]
    Pid,
    #[error("runtime app version is missing")]
    AppVersion,
    #[error("runtime protocol range is invalid")]
    ProtocolRange,
    #[error("runtime endpoint is missing")]
    Endpoint,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn discovery_round_trip_has_no_session_or_database_material() {
        let discovery = RuntimeDiscovery::new(
            Uuid::from_u128(1),
            42,
            "0.3.3",
            1,
            1,
            "/private/runtime/dopedb.sock",
            Utc::now(),
        )
        .unwrap();
        let encoded = serde_json::to_string(&discovery).unwrap();
        for forbidden in [
            "token",
            "password",
            "credential",
            "database",
            "workspace",
            "connection",
        ] {
            assert!(!encoded.to_ascii_lowercase().contains(forbidden));
        }
        let decoded: RuntimeDiscovery = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, discovery);
        assert!(!format!("{discovery:?}").contains("/private/runtime"));
    }

    #[test]
    fn unknown_or_secret_fields_fail_closed() {
        let value = json!({
            "schemaVersion": 1,
            "runtimeId": Uuid::from_u128(1),
            "pid": 42,
            "appVersion": "0.3.3",
            "protocolMin": 1,
            "protocolMax": 1,
            "endpoint": "local",
            "startedAt": Utc::now(),
            "token": "must-not-be-accepted"
        });
        assert!(serde_json::from_value::<RuntimeDiscovery>(value).is_err());
    }
}
