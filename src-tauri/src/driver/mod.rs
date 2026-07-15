//! Driver registry and runtime dispatch. The registry decides which protocol driver is
//! compatible and preferred; concrete adapters own connection mechanics. Downloadable
//! packs use the same metadata contract as bundled drivers, without pretending Rust
//! crates can be hot-loaded like JDBC jars.

use serde::{Deserialize, Serialize};

use crate::connection::pool::{connect_sqlx, LiveConnection};
use crate::connection::providers;
use crate::error::{AppError, AppResult};
use crate::model::{ConnectionProfile, Engine, Provider};

/// How a driver reaches the local installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriverInstallMode {
    /// Compiled into the signed application bundle.
    Bundled,
    /// Installed as a separately verified driver-pack sidecar.
    Managed,
}

/// Current local availability of a driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriverInstallState {
    Installed,
    Available,
    /// Listed for roadmap visibility but not downloadable or usable in this build.
    Planned,
}

/// Features exposed by a driver adapter. Higher layers ask for capabilities instead of
/// matching on driver ids, so document and graph engines can return different surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriverCapability {
    Sql,
    DocumentQuery,
    Transactions,
    Introspection,
    Collections,
    SchemaDiff,
    Monitoring,
}

/// Serializable driver metadata used by both the connection form and runtime resolver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverDescriptor {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    pub version: String,
    pub install_mode: DriverInstallMode,
    pub install_state: DriverInstallState,
    pub supported_providers: Vec<Provider>,
    pub capabilities: Vec<DriverCapability>,
    /// Registry preference among compatible drivers. Registry order breaks future ties.
    pub recommended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeAdapter {
    Postgres,
    Mysql,
    Sqlite,
}

struct DriverDefinition {
    id: &'static str,
    name: &'static str,
    engine: Engine,
    version: &'static str,
    install_mode: DriverInstallMode,
    install_state: DriverInstallState,
    supported_providers: &'static [Provider],
    capabilities: &'static [DriverCapability],
    recommended: bool,
    adapter: Option<RuntimeAdapter>,
}

const SQL_CAPABILITIES: &[DriverCapability] = &[
    DriverCapability::Sql,
    DriverCapability::Transactions,
    DriverCapability::Introspection,
    DriverCapability::SchemaDiff,
    DriverCapability::Monitoring,
];

const DEFINITIONS: &[DriverDefinition] = &[
    DriverDefinition {
        id: "sqlx-postgres",
        name: "SQLx PostgreSQL",
        engine: Engine::Postgres,
        version: "0.8",
        install_mode: DriverInstallMode::Bundled,
        install_state: DriverInstallState::Installed,
        supported_providers: &[Provider::Generic, Provider::Neon, Provider::PlanetScale],
        capabilities: SQL_CAPABILITIES,
        recommended: true,
        adapter: Some(RuntimeAdapter::Postgres),
    },
    DriverDefinition {
        id: "sqlx-mysql",
        name: "SQLx MySQL / MariaDB",
        engine: Engine::Mysql,
        version: "0.8",
        install_mode: DriverInstallMode::Bundled,
        install_state: DriverInstallState::Installed,
        supported_providers: &[Provider::Generic, Provider::PlanetScale],
        capabilities: SQL_CAPABILITIES,
        recommended: true,
        adapter: Some(RuntimeAdapter::Mysql),
    },
    DriverDefinition {
        id: "sqlx-sqlite",
        name: "SQLx SQLite",
        engine: Engine::Sqlite,
        version: "0.8",
        install_mode: DriverInstallMode::Bundled,
        install_state: DriverInstallState::Installed,
        supported_providers: &[Provider::Generic],
        capabilities: SQL_CAPABILITIES,
        recommended: true,
        adapter: Some(RuntimeAdapter::Sqlite),
    },
    DriverDefinition {
        id: "mongodb-rust",
        name: "MongoDB Rust Driver",
        engine: Engine::Mongodb,
        version: "3",
        install_mode: DriverInstallMode::Managed,
        install_state: DriverInstallState::Planned,
        supported_providers: &[Provider::Generic],
        // Capabilities describe code available now, not roadmap intent.
        capabilities: &[],
        recommended: false,
        // Keep the roadmap entry visible without presenting it as installable.
        adapter: None,
    },
];

impl DriverDefinition {
    fn descriptor(&self) -> DriverDescriptor {
        DriverDescriptor {
            id: self.id.to_string(),
            name: self.name.to_string(),
            engine: self.engine,
            version: self.version.to_string(),
            install_mode: self.install_mode,
            install_state: self.install_state,
            supported_providers: self.supported_providers.to_vec(),
            capabilities: self.capabilities.to_vec(),
            recommended: self.recommended,
        }
    }

    fn supports(&self, engine: Engine, provider: Provider) -> bool {
        self.engine == engine && self.supported_providers.contains(&provider)
    }
}

/// All known drivers in preference order. Bundled and managed packs share this shape.
pub fn list() -> Vec<DriverDescriptor> {
    DEFINITIONS
        .iter()
        .map(DriverDefinition::descriptor)
        .collect()
}

fn find(id: &str) -> AppResult<&'static DriverDefinition> {
    DEFINITIONS
        .iter()
        .find(|driver| driver.id == id)
        .ok_or_else(|| AppError::Config(format!("unknown database driver {id:?}")))
}

fn resolve(profile: &ConnectionProfile) -> AppResult<&'static DriverDefinition> {
    let provider = providers::resolve(profile);
    let selected = match profile.driver_id.as_deref() {
        Some(id) => find(id)?,
        None => DEFINITIONS
            .iter()
            .find(|driver| driver.recommended && driver.supports(profile.engine, provider))
            .ok_or_else(|| {
                AppError::Config(format!(
                    "no installed driver supports {:?} on {:?}",
                    profile.engine, provider
                ))
            })?,
    };

    if !selected.supports(profile.engine, provider) {
        return Err(AppError::Config(format!(
            "driver {:?} does not support {:?} on {:?}",
            selected.id, profile.engine, provider
        )));
    }
    match selected.install_state {
        DriverInstallState::Installed => {}
        DriverInstallState::Available => {
            return Err(AppError::Config(format!(
                "driver {:?} must be installed before connecting",
                selected.id
            )))
        }
        DriverInstallState::Planned => {
            return Err(AppError::Config(format!(
                "driver {:?} is planned but not available in this build",
                selected.id
            )))
        }
    }
    if selected.adapter.is_none() {
        return Err(AppError::Config(format!(
            "installed driver {:?} has no runtime adapter in this build",
            selected.id
        )));
    }
    Ok(selected)
}

/// Validate the selected or automatically recommended driver without opening a socket.
pub fn validate(profile: &ConnectionProfile) -> AppResult<DriverDescriptor> {
    Ok(resolve(profile)?.descriptor())
}

/// Ensure a driver is installed. Bundled drivers are already ready; managed packs will
/// route through the verified pack installer once a signed pack is added to the catalog.
pub fn install(id: &str) -> AppResult<DriverDescriptor> {
    let driver = find(id)?;
    if driver.install_state == DriverInstallState::Planned {
        return Err(AppError::Config(format!(
            "driver {:?} is planned but not available in this build",
            driver.id
        )));
    }
    match driver.install_mode {
        DriverInstallMode::Bundled => Ok(driver.descriptor()),
        DriverInstallMode::Managed => Err(AppError::Config(format!(
            "managed driver pack {:?} has no verified artifact for this build",
            driver.id
        ))),
    }
}

/// Resolve the optimal compatible adapter, then delegate connection mechanics to it.
pub async fn connect(profile: &ConnectionProfile, secret: &str) -> AppResult<LiveConnection> {
    let driver = resolve(profile)?;
    let adapter = driver.adapter.ok_or_else(|| {
        AppError::Config(format!(
            "driver {:?} has no runtime adapter in this build",
            driver.id
        ))
    })?;
    match adapter {
        RuntimeAdapter::Postgres => connect_sqlx(Engine::Postgres, profile, secret).await,
        RuntimeAdapter::Mysql => connect_sqlx(Engine::Mysql, profile, secret).await,
        RuntimeAdapter::Sqlite => connect_sqlx(Engine::Sqlite, profile, secret).await,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use super::*;

    fn profile(engine: Engine, provider: Provider) -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::new_v4(),
            name: "test".into(),
            engine,
            provider,
            driver_id: None,
            host: "localhost".into(),
            port: 5432,
            database: "db".into(),
            username: "user".into(),
            sslmode: "prefer".into(),
            extra_params: HashMap::new(),
            readonly_default: true,
            allow_writes: false,
            secret_ref: None,
            env: None,
            schema_group: None,
        }
    }

    #[test]
    fn recommends_provider_compatible_driver() {
        let neon = validate(&profile(Engine::Postgres, Provider::Neon)).unwrap();
        assert_eq!(neon.id, "sqlx-postgres");

        let planetscale = validate(&profile(Engine::Mysql, Provider::PlanetScale)).unwrap();
        assert_eq!(planetscale.id, "sqlx-mysql");
    }

    #[test]
    fn rejects_incompatible_provider_and_engine() {
        let err = validate(&profile(Engine::Mysql, Provider::Neon)).unwrap_err();
        assert!(err.to_string().contains("no installed driver"));
    }

    #[test]
    fn rejects_explicit_driver_for_wrong_engine() {
        let mut p = profile(Engine::Postgres, Provider::Generic);
        p.driver_id = Some("sqlx-mysql".into());
        assert!(validate(&p).is_err());
    }

    #[test]
    fn lists_mongodb_as_planned_without_runtime_capabilities() {
        let mongo = list()
            .into_iter()
            .find(|driver| driver.id == "mongodb-rust")
            .unwrap();
        assert_eq!(mongo.engine, Engine::Mongodb);
        assert_eq!(mongo.install_state, DriverInstallState::Planned);
        assert!(mongo.capabilities.is_empty());
        assert!(!mongo.recommended);
    }

    #[test]
    fn mongodb_cannot_install_or_connect_before_the_adapter_exists() {
        let mut p = profile(Engine::Mongodb, Provider::Generic);
        p.driver_id = Some("mongodb-rust".into());
        let validate_err = validate(&p).unwrap_err();
        let install_err = install("mongodb-rust").unwrap_err();
        assert!(validate_err.to_string().contains("planned"));
        assert!(install_err.to_string().contains("planned"));
    }
}
