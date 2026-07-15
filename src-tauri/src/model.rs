//! Shared serde types — the data contract between the Rust core and the React
//! frontend. All types serialize `camelCase`. Keep this file authoritative:
//! module agents conform to these shapes rather than redefining them.
//!
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported target database engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Engine {
    Postgres,
    Mysql,
    Sqlite,
    Mongodb,
}

/// Hosting/control-plane provider. `Auto` preserves connection-URL convenience while
/// keeping provider-specific behavior separate from the database wire protocol.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    #[default]
    Auto,
    Generic,
    Neon,
    PlanetScale,
}

/// A saved connection. Secrets never live here — only a `secretRef` pointing at the
/// OS credential-store item that holds the password/connection string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub engine: Engine,
    /// Provider overlay selected by the user; `Auto` resolves from the endpoint.
    #[serde(default)]
    pub provider: Provider,
    /// Explicit driver selection. `None` asks the registry for its best compatible driver.
    #[serde(default)]
    pub driver_id: Option<String>,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub sslmode: String,
    #[serde(default)]
    pub extra_params: HashMap<String, String>,
    /// Open connections read-only by default.
    pub readonly_default: bool,
    /// Master per-connection gate for the write path (default false).
    pub allow_writes: bool,
    /// Credential-store item id for the secret, if one has been stored.
    pub secret_ref: Option<String>,
    /// Environment label ("dev" | "staging" | "prod") — drives the sidebar/header chip.
    #[serde(default)]
    pub env: Option<String>,
    /// Shared schema family. Connections with the same value are compared as
    /// dev/staging/prod siblings, using prod as the default baseline when present.
    #[serde(default)]
    pub schema_group: Option<String>,
}

/// Per-connection safety configuration (mirrors `connection_safety` in app.db).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetySettings {
    pub require_approval: bool,
    pub allow_writes: bool,
    pub wrap_writes_in_tx: bool,
    pub explain_preview: bool,
    pub auto_run_reads: bool,
    /// Row cap applied to read result sets.
    pub max_rows: u64,
    /// L3 gate (design-review #4): skip execute-preview when the EXPLAIN row estimate
    /// exceeds this and show the estimate only ("would lock ~N rows").
    pub exec_preview_row_limit: i64,
}

/// Monitoring capability exposed by one saved connection. PostgreSQL can opt in to
/// the built-in `pg_monitor` role; other engines keep a basic, role-free collector.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringStatus {
    pub engine: Engine,
    /// "full" when pg_monitor is granted, "limited" without it, "basic" for
    /// engines that do not use PostgreSQL's predefined monitoring roles.
    pub coverage: String,
    pub role_available: bool,
    pub role_granted: bool,
    pub current_user: Option<String>,
    /// Best-effort hint only. The server remains authoritative when GRANT/REVOKE runs.
    pub can_manage: bool,
    pub note: String,
}

impl Default for SafetySettings {
    fn default() -> Self {
        SafetySettings {
            require_approval: true,
            allow_writes: false,
            wrap_writes_in_tx: true,
            explain_preview: true,
            auto_run_reads: true,
            max_rows: 1000,
            exec_preview_row_limit: 50_000,
        }
    }
}

/// Statement class from L1 parse/classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueryKind {
    Read,
    Write,
    Ddl,
    Privilege,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// Result of L1 classification. A UX pre-filter — L2 is the authoritative boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub kind: QueryKind,
    pub risk: RiskLevel,
    /// Number of top-level statements parsed. `> 1` is rejected.
    pub statement_count: u32,
    /// UPDATE/DELETE without a WHERE clause (high-risk flag).
    pub no_where: bool,
    pub tables: Vec<String>,
    pub notes: Vec<String>,
    /// True ONLY for exactly one cleanly-parsed top-level INSERT/UPDATE/DELETE —
    /// i.e. a statement the L3 execute+ROLLBACK preview can undo. DDL/utility
    /// statements implicit-commit (RENAME/OPTIMIZE/LOAD DATA…), so ROLLBACK is a
    /// no-op and the preview would take permanent effect BEFORE L4 approval.
    /// Fail-safe/parse-error/multi-statement writes are false. Gates l3_preview.
    #[serde(default)]
    pub rollback_safe: bool,
}

/// How an impact preview was produced (L3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewMode {
    /// Read path: EXPLAIN plan only, never executed.
    Explain,
    /// Write path: executed in a txn then unconditionally rolled back for exact N.
    ExecRollback,
    /// Execute-preview skipped (estimate over threshold); estimate shown only.
    Skipped,
}

/// L3 impact preview shown on the approval card.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReport {
    pub mode: PreviewMode,
    /// EXPLAIN-derived row estimate.
    pub estimated_rows: Option<i64>,
    /// Exact rows_affected from the execute+rollback path.
    pub exact_rows: Option<i64>,
    /// Raw/formatted plan text, if captured.
    pub plan: Option<String>,
    /// Human note, e.g. "would lock ~120000 rows — preview skipped".
    pub note: Option<String>,
}

/// A materialized result set (or a page of one).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    /// True if the result was cut off at the row cap.
    pub truncated: bool,
    pub duration_ms: u64,
}

/// Supported saved-dashboard renderers. `Auto` lets the client choose from the
/// returned result shape; the other variants are explicit user/agent choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub enum DashboardKind {
    #[default]
    Auto,
    Metric,
    Line,
    Bar,
    Table,
}

fn dashboard_visualization_version() -> u32 {
    1
}

/// Versioned, declarative visualization metadata. The app renders this trusted
/// shape itself; generated HTML is deliberately not part of the persistence model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardVisualization {
    #[serde(default = "dashboard_visualization_version")]
    pub version: u32,
    pub kind: DashboardKind,
    pub x_column: Option<String>,
    #[serde(default)]
    pub y_columns: Vec<String>,
}

/// One persistent saved query + visualization, scoped to a connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub title: String,
    pub description: String,
    pub sql: String,
    pub visualization: DashboardVisualization,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Create input for a dashboard. IDs and timestamps are assigned by the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDraft {
    pub connection_id: Uuid,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub sql: String,
    pub visualization: DashboardVisualization,
}

/// Outcome of a `run_sql` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOutcome {
    pub result: Option<QueryResult>,
    pub affected: Option<u64>,
    /// True only when a write actually committed.
    pub committed: bool,
}

/// One statement's outcome inside a `run_script` run. Exactly one of `result`/
/// `affected`/`error` is meaningful: a read carries `result`, a write carries
/// `affected`, a failed or skipped statement carries `error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptStatement {
    pub sql: String,
    pub result: Option<QueryResult>,
    pub affected: Option<i64>,
    pub error: Option<String>,
}

/// Outcome of a `run_script` call. `committed` is true only for a write script whose
/// single transaction committed; `all_reads` picks the read-only sequential path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptOutcome {
    pub statements: Vec<ScriptStatement>,
    pub committed: bool,
    pub all_reads: bool,
}

/// One append-only, hash-chained audit record (compliance log).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub ts: DateTime<Utc>,
    pub engine: Engine,
    pub agent_prompt: Option<String>,
    pub sql: String,
    pub kind: QueryKind,
    /// e.g. "propose" | "approve" | "reject" | "execute" | "blocked".
    pub action: String,
    pub approved_by: Option<String>,
    pub affected_estimate: Option<i64>,
    pub error: Option<String>,
    pub prev_hash: Option<String>,
    /// SHA256(prev_hash ‖ canonical_row) — tamper-evidence chain link.
    pub hash: String,
}

/// One `query_history` row (UX/replay log, kept separate from the audit log).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub sql: String,
    pub kind: QueryKind,
    /// "ok" | "error" | "blocked".
    pub status: String,
    pub row_count: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub executed_at: DateTime<Utc>,
    /// "agent" | "manual" | "dashboard" | "migration".
    pub origin: String,
}
