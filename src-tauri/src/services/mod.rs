//! Transport-neutral application services shared by Tauri, MCP, and future local
//! broker adapters. Services expose domain DTOs and errors, never transport types.

mod catalog_service;
mod connection_service;
mod dashboard_service;

pub(crate) use catalog_service::{CatalogReadPolicy, CatalogService};
pub(crate) use connection_service::{
    AgentConnectionSummary, ConnectionService, LegacyConnectionResolutionError,
};
pub(crate) use dashboard_service::{
    AgentDashboardCommitError, AgentDashboardPrepareError, AgentDashboardPresentation,
    DashboardService,
};

use crate::connection::ConnectionManager;
use crate::store::Store;

/// Cloneable application-service facade. Every clone retains the same local store and
/// scope-aware connection runtime, so every service method uses one authority boundary.
#[derive(Clone)]
pub(crate) struct ApplicationServices {
    pub(crate) connections: ConnectionService,
    pub(crate) catalog: CatalogService,
    pub(crate) dashboard: DashboardService,
}

impl ApplicationServices {
    pub(crate) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self {
            connections: ConnectionService::new(store.clone()),
            catalog: CatalogService::new(store.clone(), connections.clone()),
            dashboard: DashboardService::new(store, connections),
        }
    }
}
