//! Transport-neutral application services shared by Tauri, MCP, and future local
//! broker adapters. Services expose domain DTOs and errors, never transport types.

mod activity_service;
mod catalog_service;
mod connection_credentials;
mod connection_service;
mod dashboard_service;
mod document_service;
mod monitoring_service;
mod query_service;
mod safety_service;
mod script_service;
mod workspace_service;

pub(crate) use activity_service::{ActivityService, AuditSnapshotReceipt, AuditVerdict};
pub(crate) use catalog_service::{CatalogReadPolicy, CatalogService};
pub(crate) use connection_service::{
    AgentConnectionSummary, ConnectionProfileTestRequest, ConnectionService,
    ConnectionUpsertRequest, LegacyConnectionResolutionError,
};
pub(crate) use dashboard_service::{
    AgentDashboardCommitError, AgentDashboardPrepareError, AgentDashboardPresentation,
    DashboardRunError, DashboardRunReceipt, DashboardRunRequest, DashboardService,
};
pub(crate) use document_service::{
    AgentDocumentReadError, AgentDocumentReadRequest, DesktopDocumentReadError,
    DesktopDocumentReadRequest, DocumentReadReceipt, DocumentService,
};
pub(crate) use monitoring_service::{
    MonitoringChangeRequest, MonitoringService, MonitoringServiceError, MonitoringStatusReceipt,
};
#[cfg(test)]
pub(crate) use query_service::{planning_guidance, MAX_AGENT_ROWS};
pub(crate) use query_service::{
    AgentQueryInvocationOrigin, AgentQueryPlanError, AgentQueryPlanRequest, AgentQueryRunError,
    AgentQueryRunPrepareError, DesktopSqlClassificationReceipt, DesktopSqlClassificationRequest,
    DesktopSqlInspectionError, DesktopSqlPreviewReceipt, DesktopSqlPreviewRequest,
    DesktopSqlRunError, DesktopSqlRunReceipt, DesktopSqlRunRequest, QueryService, QUERY_PLAN_TTL,
};
pub(crate) use safety_service::SafetyService;
pub(crate) use script_service::{
    DesktopScriptRunError, DesktopScriptRunReceipt, DesktopScriptRunRequest, ScriptService,
};
pub(crate) use workspace_service::{
    WorkspaceConnectionCopyRequest, WorkspaceCredentialBindingRequest, WorkspaceService,
};

use crate::connection::ConnectionManager;
use crate::store::Store;

/// Cloneable application-service facade. Every clone retains the same local store and
/// scope-aware connection runtime, so every service method uses one authority boundary.
#[derive(Clone)]
pub(crate) struct ApplicationServices {
    pub(crate) activity: ActivityService,
    pub(crate) connections: ConnectionService,
    pub(crate) catalog: CatalogService,
    pub(crate) dashboard: DashboardService,
    pub(crate) document: DocumentService,
    pub(crate) monitoring: MonitoringService,
    pub(crate) query: QueryService,
    pub(crate) safety: SafetyService,
    pub(crate) script: ScriptService,
    pub(crate) workspace: WorkspaceService,
}

impl ApplicationServices {
    pub(crate) fn new(store: Store, connections: ConnectionManager) -> Self {
        let connection_credentials = connection_credentials::system_connection_credentials();
        Self {
            activity: ActivityService::new(store.clone()),
            connections: ConnectionService::new(
                store.clone(),
                connections.clone(),
                connection_credentials.clone(),
            ),
            catalog: CatalogService::new(store.clone(), connections.clone()),
            dashboard: DashboardService::new(store.clone(), connections.clone()),
            document: DocumentService::new(store.clone(), connections.clone()),
            monitoring: MonitoringService::new(store.clone(), connections.clone()),
            query: QueryService::new(store.clone(), connections.clone()),
            safety: SafetyService::new(store.clone(), connections.clone()),
            script: ScriptService::new(store.clone(), connections.clone()),
            workspace: WorkspaceService::new(store, connections, connection_credentials),
        }
    }
}
