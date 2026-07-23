// TS mirrors of the Rust `model.rs` serde types. All shapes serialize camelCase.
// Keep this file in lockstep with src-tauri/src/model.rs — it is the data contract.

export type Engine = "postgres" | "mysql" | "sqlite" | "mongodb";
export type Provider = "auto" | "generic" | "neon" | "planetScale" | "gcpCloudSql";

type WorkspaceKind = "personal" | "team";
type WorkspaceLifecycleState = "active" | "archived" | "deleted";
export type WorkspaceRole = "viewer" | "analyst" | "editor" | "admin" | "owner";
type WorkspaceConnectionAccess = "view" | "read" | "write" | "manage" | "local";
export type WorkspaceCredentialMode = "local" | "memberLocal" | "managed";

export interface Workspace {
  id: string;
  name: string;
  kind: WorkspaceKind;
  lifecycleState: WorkspaceLifecycleState;
  createdAt: string;
  updatedAt: string;
}

export interface WorkspaceFeatureState {
  enabled: boolean;
}

interface WorkspaceAuthUser {
  id: string;
  email: string;
  displayName: string;
}

interface WorkspaceAccountMembership {
  workspaceId: string;
  role: WorkspaceRole;
}

interface WorkspaceAuthAccount {
  user: WorkspaceAuthUser;
  memberships: WorkspaceAccountMembership[];
}

export interface WorkspaceAuthState {
  authenticated: boolean;
  user: WorkspaceAuthUser | null;
  accounts: WorkspaceAuthAccount[];
}

export interface WorkspaceDeviceAuthorization {
  deviceCode: string;
  userCode: string;
  verificationUriComplete: string;
  expiresIn: number;
  interval: number;
}

type WorkspaceLoginPollStatus =
  | "pending"
  | "slowDown"
  | "signedIn"
  | "denied"
  | "expired";

export interface WorkspaceLoginPoll {
  status: WorkspaceLoginPollStatus;
  user: WorkspaceAuthUser | null;
}

export interface ConnectionProfile {
  id: string; // Uuid
  name: string;
  engine: Engine;
  provider: Provider;
  driverId: string | null;
  host: string;
  port: number;
  database: string;
  username: string;
  sslmode: string;
  extraParams: Record<string, string>;
  readonlyDefault: boolean;
  allowWrites: boolean;
  secretRef: string | null;
  env: string | null; // "dev" | "staging" | "prod" | null
  schemaGroup: string | null; // shared group for dev/staging/prod schema comparison
  workspaceAccess: WorkspaceConnectionAccess;
  credentialMode: WorkspaceCredentialMode;
}

// Mirrors src-tauri/src/driver/mod.rs.
type DriverInstallMode = "bundled" | "managed";
type DriverInstallState = "installed" | "available" | "planned";
export type DriverCapability =
  | "sql"
  | "documentQuery"
  | "transactions"
  | "introspection"
  | "collections"
  | "schemaDiff"
  | "monitoring";

export interface DriverDescriptor {
  id: string;
  name: string;
  engine: Engine;
  version: string;
  installMode: DriverInstallMode;
  installState: DriverInstallState;
  supportedProviders: Provider[];
  capabilities: DriverCapability[];
  recommended: boolean;
}

export interface SafetySettings {
  requireApproval: boolean;
  allowWrites: boolean;
  wrapWritesInTx: boolean;
  explainPreview: boolean;
  autoRunReads: boolean;
  maxRows: number;
  execPreviewRowLimit: number;
}

export interface MonitoringStatus {
  engine: Engine;
  coverage: "full" | "limited" | "basic";
  roleAvailable: boolean;
  roleGranted: boolean;
  currentUser: string | null;
  canManage: boolean;
  note: string;
}

type QueryKind = "read" | "write" | "ddl" | "privilege";

export type RiskLevel = "low" | "medium" | "high";

export interface Classification {
  kind: QueryKind;
  risk: RiskLevel;
  statementCount: number;
  noWhere: boolean;
  tables: string[];
  notes: string[];
  /** True only for a single cleanly-parsed write the L3 exec+ROLLBACK preview can undo. */
  rollbackSafe: boolean;
}

type PreviewMode = "explain" | "execRollback" | "skipped";

export interface PreviewReport {
  mode: PreviewMode;
  estimatedRows: number | null;
  exactRows: number | null;
  plan: string | null;
  note: string | null;
}

export interface QueryResult {
  columns: string[];
  rows: unknown[][];
  rowCount: number;
  truncated: boolean;
  durationMs: number;
}

// One typed, read-only MongoDB request (mirrors model.rs DocumentQuery). Filters,
// projections, sorts, and pipeline stages accept MongoDB Extended JSON objects.
export type DocumentQuery =
  | {
      op: "find";
      collection: string;
      filter?: unknown;
      projection?: unknown;
      sort?: unknown;
      skip?: number;
      limit?: number;
    }
  | { op: "aggregate"; collection: string; pipeline: unknown[] }
  | { op: "count"; collection: string; filter?: unknown };

// A page of documents from one DocumentQuery run; each element is one BSON
// document as relaxed Extended JSON (mirrors model.rs DocumentPage).
export interface DocumentPage {
  documents: unknown[];
  docCount: number;
  truncated: boolean;
  durationMs: number;
}

export type DashboardKind = "auto" | "metric" | "line" | "bar" | "table";

export interface DashboardVisualization {
  version: 1;
  kind: DashboardKind;
  xColumn: string | null;
  yColumns: string[];
}

export interface Dashboard {
  id: string;
  connectionId: string;
  title: string;
  description: string;
  sql: string;
  visualization: DashboardVisualization;
  createdAt: string;
  updatedAt: string;
}

export interface DashboardDraft {
  connectionId: string;
  title: string;
  description: string;
  sql: string;
  visualization: DashboardVisualization;
}

export interface ExecOutcome {
  result: QueryResult | null;
  affected: number | null;
  committed: boolean;
}

// One statement's outcome inside a run_script run. A read carries `result`, a write
// carries `affected`, a failed/skipped statement carries `error`.
interface ScriptStatement {
  sql: string;
  result: QueryResult | null;
  affected: number | null;
  error: string | null;
}

export interface ScriptOutcome {
  statements: ScriptStatement[];
  committed: boolean; // true only when a write script's transaction committed
  allReads: boolean; // true when the read-only sequential path ran
}

interface AuditEntry {
  id: string;
  connectionId: string;
  ts: string; // ISO-8601
  engine: Engine;
  agentPrompt: string | null;
  sql: string;
  kind: QueryKind;
  action: string;
  approvedBy: string | null;
  affectedEstimate: number | null;
  error: string | null;
  prevHash: string | null;
  hash: string;
}

interface AuditVerdict {
  ok: boolean;
  firstBadIndex: number | null;
}

export interface AuditSnapshot {
  entries: AuditEntry[];
  verdict: AuditVerdict;
}

export interface HistoryEntry {
  id: string;
  connectionId: string;
  sql: string;
  kind: QueryKind;
  status: string;
  rowCount: number | null;
  durationMs: number | null;
  error: string | null;
  executedAt: string;
  origin: string;
}

// Schema introspection (mirrors src-tauri/src/introspect/mod.rs Catalog).
export interface CatalogColumn {
  name: string;
  dataType: string;
  nullable: boolean;
  pk: boolean;
}

interface CatalogForeignKey {
  column: string;
  referencesTable: string;
  referencesColumn: string;
  referencesSchema: string | null;
}

interface CatalogIndex {
  name: string;
  columns: string[];
  unique: boolean;
}

export interface CatalogTable {
  schema: string | null;
  name: string;
  kind: string; // "table" | "view"
  columns: CatalogColumn[];
  foreignKeys: CatalogForeignKey[];
  indexes: CatalogIndex[];
  rowEstimate: number | null;
}

export type CatalogObjectKind =
  | "function"
  | "procedure"
  | "trigger"
  | "sequence"
  | "materialized_view";

export interface CatalogObject {
  schema: string | null;
  name: string;
  kind: CatalogObjectKind | string;
  detail?: string | null;
  parent?: string | null;
}

export interface Catalog {
  tables: CatalogTable[];
  // Optional while schema caches created by older app versions are still present.
  objects?: CatalogObject[];
}

// One-click connect: an AI platform dopedb can wire up (mirrors mcp/connect.rs).
export interface PlatformInfo {
  id: string;
  name: string;
  installed: boolean;
  connected: boolean; // dopedb entry already present in the platform's MCP config
  method: string; // "http" | "bridge"
  note: string;
}

// In-app agent chat: which subscription CLI a turn runs through, and its installed/
// authenticated status. Mirrors src-tauri/src/agent/mod.rs.
export type AgentProvider = "claude" | "codex";

export interface CliInfo {
  id: AgentProvider;
  name: string;
  installed: boolean;
  authenticated: boolean;
  authMethod: string | null;
  // Present on the wire but deliberately unused for display — the onboarding card renders
  // a per-provider i18n string instead (src/screens/AgentChat/index.tsx PROVIDER_NOTE_KEYS)
  // so the subscription-login disclosure follows the app's language, not the backend's.
  note: string;
}

// One selectable model for a provider's chat composer (codex's own catalog, or a static
// list for claude, which has none). Mirrors src-tauri/src/agent/mod.rs.
export interface AgentModel {
  id: string;
  name: string;
  efforts: string[];
  defaultEffort: string | null;
}

// A persisted conversation (Store/SQLite `agent_chat_threads`). model/effort are the values
// used by the thread's most recent turn, seeded back into the composer when it's reopened.
// connectionId is null for an unscoped thread (pre-dating connection scoping, or explicitly
// started without one) — never used to pre-fill send_turn's context block in that case.
// Mirrors src-tauri/src/agent/mod.rs.
export interface ChatThread {
  id: string;
  provider: AgentProvider;
  connectionId: string | null;
  title: string;
  cliSessionId: string | null;
  model: string | null;
  effort: string | null;
  createdAt: string;
  updatedAt: string;
}

// One persisted message row (Store/SQLite `agent_chat_messages`). Mirrors
// src-tauri/src/agent/mod.rs.
export interface ChatMessageRecord {
  id: string;
  threadId: string;
  role: "user" | "assistant";
  text: string;
  error: string | null;
  createdAt: string;
}

// The `{ kind, message, position? }` object AppError serializes to.
interface AppErrorShape {
  kind: string;
  message: string;
  /** 1-based character offset into the executed SQL (Postgres only). */
  position?: number;
}

export function errMessage(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) {
    return String((e as AppErrorShape).message);
  }
  return String(e);
}

export interface AppErrorDetails {
  kind: string | null;
  message: string;
  position: number | null;
  raw: string;
}

export function errDetails(e: unknown): AppErrorDetails {
  if (e && typeof e === "object" && "message" in e) {
    const shaped = e as Partial<AppErrorShape>;
    let raw = String(e);
    try {
      raw = JSON.stringify(e, null, 2) ?? raw;
    } catch {
      // Fall back to String(e).
    }
    return {
      kind: typeof shaped.kind === "string" ? shaped.kind : null,
      message: String(shaped.message),
      position: typeof shaped.position === "number" ? shaped.position : null,
      raw,
    };
  }
  return { kind: null, message: String(e), position: null, raw: String(e) };
}
