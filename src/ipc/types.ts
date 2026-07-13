// TS mirrors of the Rust `model.rs` serde types. All shapes serialize camelCase.
// Keep this file in lockstep with src-tauri/src/model.rs — it is the data contract.

export type Engine = "postgres" | "mysql" | "sqlite";

export interface ConnectionProfile {
  id: string; // Uuid
  name: string;
  engine: Engine;
  host: string;
  port: number;
  database: string;
  username: string;
  sslmode: string;
  extraParams: Record<string, string>;
  readonlyDefault: boolean;
  allowWrites: boolean;
  secretRef: string | null;
  projectDir: string | null;
  env: string | null; // "dev" | "staging" | "prod" | null
  schemaGroup: string | null; // shared group for dev/staging/prod schema comparison
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

export interface AuditEntry {
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

export interface AuditVerdict {
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

export interface Catalog {
  tables: CatalogTable[];
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

// Migration change-log (mirrors src-tauri/src/migrations/mod.rs).
interface ChangeView {
  kind: string;
  summary: string;
  down: string | null;
  reversible: boolean;
}
export interface MigrationView {
  version: string;
  name: string;
  upFile: string;
  hasDownFile: boolean;
  changes: ChangeView[];
  generatedDown: string;
  parseError: string | null;
  // Additive fields from the applied-state backend stage.
  partialParse: boolean;
  applied: boolean | null; // null when no connection/tracker detected
  applyScript: string | null; // up SQL + tracking mark (always populated)
  rollbackScript: string | null; // down SQL + tracking un-mark (always populated)
}
interface ColumnDiff {
  table: string;
  missingInDb: string[];
  extraInDb: string[];
}
interface Drift {
  pendingTables: string[];
  extraTables: string[];
  columnDiffs: ColumnDiff[];
}
export interface MigrationReport {
  dir: string;
  migrations: MigrationView[];
  drift: Drift | null;
  error: string | null;
  tracker: string | null; // "prisma" | "sqlx" | "rails" | "golang-migrate" | "flyway" | "drizzle"
  trackerTable: string | null;
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
