//! Transport-neutral application services shared by Tauri, MCP, and future local
//! broker adapters. Services expose domain DTOs and errors, never transport types.

mod catalog_service;
mod connection_service;
mod dashboard_service;
mod document_service;
mod query_service;

pub(crate) use catalog_service::{CatalogReadPolicy, CatalogService};
pub(crate) use connection_service::{
    AgentConnectionSummary, ConnectionService, LegacyConnectionResolutionError,
};
pub(crate) use dashboard_service::{
    AgentDashboardCommitError, AgentDashboardPrepareError, AgentDashboardPresentation,
    DashboardService,
};
pub(crate) use document_service::{
    AgentDocumentReadError, AgentDocumentReadRequest, DesktopDocumentReadError,
    DesktopDocumentReadRequest, DocumentReadReceipt, DocumentService,
};
#[cfg(test)]
pub(crate) use query_service::{planning_guidance, MAX_AGENT_ROWS};
pub(crate) use query_service::{
    AgentQueryInvocationOrigin, AgentQueryPlanError, AgentQueryPlanRequest, AgentQueryRunError,
    AgentQueryRunPrepareError, DesktopSqlClassificationReceipt, DesktopSqlClassificationRequest,
    DesktopSqlInspectionError, DesktopSqlPreviewReceipt, DesktopSqlPreviewRequest,
    DesktopSqlRunError, DesktopSqlRunReceipt, DesktopSqlRunRequest, QueryService, QUERY_PLAN_TTL,
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
    pub(crate) document: DocumentService,
    pub(crate) query: QueryService,
}

impl ApplicationServices {
    pub(crate) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self {
            connections: ConnectionService::new(store.clone()),
            catalog: CatalogService::new(store.clone(), connections.clone()),
            dashboard: DashboardService::new(store.clone(), connections.clone()),
            document: DocumentService::new(store.clone(), connections.clone()),
            query: QueryService::new(store, connections),
        }
    }
}
