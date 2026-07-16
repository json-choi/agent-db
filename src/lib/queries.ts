// Query-key factory and shared query options for every cached backend read. Screens
// consume these via useQuery/useQueries so one fetch per (resource, connection) is shared
// app-wide: re-entering a tab repaints from cache and revalidates in the background.
// Invalidation lives in queryClient.tsx; nothing here fetches on its own.
import { queryOptions } from "@tanstack/react-query";
import {
  auditSnapshot,
  auditVerify,
  cancelQuery,
  detectAgentClis,
  getCatalog,
  getChatMessages,
  getMonitoringStatus,
  listAgentModels,
  listChatThreads,
  listDashboards,
  listDrivers,
  listHistory,
  refreshCatalog,
  runDashboard,
  runDocumentQuery,
  runSql,
} from "../ipc/commands";
import { errMessage } from "../ipc/types";
import type { AgentProvider, CatalogTable, Engine, QueryResult } from "../ipc/types";
import { buildCountQuery, buildPageQuery, type GridSort } from "./sqlBuild";
import { tableKey } from "./tableRef";

// Introspection is written to a backend cache that never expires, so the catalog only
// needs refetching when the user explicitly refreshes it (see invalidateCatalog).
const CATALOG_STALE_MS = Infinity;
// Logs and row data are cheap to re-read. Repainting from cache is instant either way;
// this only suppresses a redundant refetch when a user flips between two tabs quickly.
const LOG_STALE_MS = 10_000;
const SCHEMA_LOAD_TIMEOUT_MS = 12_000;

function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  let timer: number | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = window.setTimeout(() => reject(new Error(message)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => window.clearTimeout(timer));
}

// Network-shaped failures (dropped Wi-Fi, DB briefly unreachable) heal on their own; a
// stuck error card that needs a manual "retry" click after the network recovers is pure
// friction. Everything else (bad credentials, invalid SQL, missing table) is deterministic
// and must keep failing fast — retrying it only doubles the wait.
const TRANSIENT_ERROR =
  /pool timed out|timed out|timeout|connection (refused|reset|closed|aborted)|could not connect|unreachable|broken pipe|network|io error/i;

export function isTransientDbError(e: unknown): boolean {
  return TRANSIENT_ERROR.test(errMessage(e));
}

// Spread into read-only queries that hit a remote database. Deliberately NOT a global
// default: runSql-backed queries (tableRows, dashboardRun) double-write query history on a
// retried run — see the `retry: false` rationale in queryClient.tsx.
const transientRetry = {
  retry: (failureCount: number, error: unknown) =>
    failureCount < 3 && isTransientDbError(error),
  retryDelay: (attempt: number) => Math.min(1000 * 2 ** attempt, 8_000),
} as const;

export type TableRowsPage = { result: QueryResult | null; total: number | null };

export type DocumentRowsArgs = {
  connectionId: string;
  collection: string;
  pageSize: number;
  page: number;
};

export type TableRowsArgs = {
  connectionId: string;
  engine: Engine;
  table: CatalogTable;
  filters: Record<string, string>;
  sort: GridSort | null;
  pageSize: number;
  page: number;
};

// Every key starts with a resource segment plus the connection id, so a connection-scoped
// invalidation is a prefix match and never has to enumerate sub-resources.
export const qk = {
  catalog: (connectionId: string) => ["catalog", connectionId] as const,
  history: (connectionId: string) => ["history", connectionId] as const,
  audit: (connectionId: string) => ["audit", connectionId] as const,
  auditVerdict: (connectionId: string) => ["audit", connectionId, "verdict"] as const,
  auditSnapshot: (connectionId: string) => ["audit", connectionId, "snapshot"] as const,
  monitoring: (connectionId: string) => ["monitoring", connectionId] as const,
  dashboards: (connectionId: string) => ["dashboards", connectionId] as const,
  dashboardRun: (dashboardId: string) => ["dashboardRun", dashboardId] as const,
  drivers: () => ["drivers"] as const,
  chatThreads: () => ["chatThreads"] as const,
  chatMessages: (threadId: string) => ["chatMessages", threadId] as const,
  agentModels: (provider: AgentProvider) => ["agentModels", provider] as const,
  tableRows: (args: TableRowsArgs) =>
    [
      "tableRows",
      args.connectionId,
      tableKey(args.table),
      { filters: args.filters, sort: args.sort, pageSize: args.pageSize, page: args.page },
    ] as const,
  documentRows: (args: DocumentRowsArgs) =>
    [
      "documentRows",
      args.connectionId,
      args.collection,
      { pageSize: args.pageSize, page: args.page },
    ] as const,
  documentCount: (connectionId: string, collection: string) =>
    ["documentCount", connectionId, collection] as const,
};

export function driversQuery() {
  return queryOptions({
    queryKey: qk.drivers(),
    staleTime: Infinity,
    queryFn: listDrivers,
  });
}

// Agent chat CLI detection (installed/authenticated). Short staleTime so the gate's
// "check again" (an explicit refetch) feels immediate, but a re-render doesn't re-spawn
// `claude`/`codex` on every keystroke.
export function agentCliDetectionQuery() {
  return queryOptions({
    queryKey: ["agentClis"] as const,
    staleTime: 15_000,
    queryFn: detectAgentClis,
  });
}

// Sidebar thread list. Small and cheap to re-read, so a short staleTime is enough to make a
// just-created thread (or a title set after the first turn) show up on the next paint.
export function agentChatThreadsQuery() {
  return queryOptions({
    queryKey: qk.chatThreads(),
    staleTime: 5_000,
    queryFn: listChatThreads,
  });
}

// One thread's message history. Disabled for a still-draft conversation (threadId null) —
// there is nothing to read until create_chat_thread has run.
export function agentChatMessagesQuery(threadId: string | null) {
  return queryOptions({
    queryKey: qk.chatMessages(threadId ?? ""),
    enabled: threadId !== null,
    staleTime: 5_000,
    queryFn: () => getChatMessages(threadId!),
  });
}

// Model catalog for the composer's picker. codex re-spawns its CLI to read this and claude's
// list is static either way, so a several-minute staleTime avoids a re-spawn per keystroke.
export function agentModelsQuery(provider: AgentProvider) {
  return queryOptions({
    queryKey: qk.agentModels(provider),
    staleTime: 5 * 60_000,
    queryFn: () => listAgentModels(provider),
  });
}

export function catalogQuery(connectionId: string) {
  return queryOptions({
    queryKey: qk.catalog(connectionId),
    staleTime: CATALOG_STALE_MS,
    ...transientRetry,
    queryFn: () =>
      withTimeout(
        getCatalog(connectionId),
        SCHEMA_LOAD_TIMEOUT_MS,
        "Schema loading timed out. Check the database connection or retry.",
      ),
  });
}

// Force a live re-introspection. The caller writes the result into qk.catalog(id) so every
// surface reading the catalog updates at once; a CATALOG_STALE_MS of Infinity means this
// is the only way a stale table list gets corrected.
export function fetchFreshCatalog(connectionId: string) {
  return withTimeout(
    refreshCatalog(connectionId),
    SCHEMA_LOAD_TIMEOUT_MS,
    "Schema refresh timed out. Check the database connection or retry.",
  );
}

export function historyQuery(connectionId: string) {
  return queryOptions({
    queryKey: qk.history(connectionId),
    staleTime: LOG_STALE_MS,
    queryFn: () => listHistory(connectionId),
  });
}

export function monitoringStatusQuery(connectionId: string) {
  return queryOptions({
    queryKey: qk.monitoring(connectionId),
    staleTime: LOG_STALE_MS,
    ...transientRetry,
    queryFn: () => getMonitoringStatus(connectionId),
  });
}

// Verification alone, for the collapsed Activity banner. The full row list can be large,
// so it stays behind auditSnapshotQuery until the disclosure is opened.
export function auditVerdictQuery(connectionId: string) {
  return queryOptions({
    queryKey: qk.auditVerdict(connectionId),
    staleTime: LOG_STALE_MS,
    queryFn: () => auditVerify(connectionId),
  });
}

export function auditSnapshotQuery(connectionId: string, enabled: boolean) {
  return queryOptions({
    queryKey: qk.auditSnapshot(connectionId),
    enabled,
    staleTime: LOG_STALE_MS,
    queryFn: () => auditSnapshot(connectionId),
  });
}

export function dashboardsQuery(connectionId: string) {
  return queryOptions({
    queryKey: qk.dashboards(connectionId),
    staleTime: LOG_STALE_MS,
    queryFn: () => listDashboards(connectionId),
  });
}

// A dashboard rerun is a read against the live database, so it is cached until the user
// asks for a fresh run. The AbortSignal is wired to the backend's cancel_query so a
// superseded or explicitly cancelled run stops server-side instead of finishing unseen.
export function dashboardRunQuery(dashboardId: string | null) {
  return queryOptions({
    queryKey: qk.dashboardRun(dashboardId ?? ""),
    enabled: dashboardId !== null,
    staleTime: Infinity,
    queryFn: ({ signal }) => {
      const queryId = window.crypto.randomUUID();
      signal.addEventListener("abort", () => void cancelQuery(queryId), { once: true });
      return runDashboard(dashboardId!, queryId);
    },
  });
}

// One page of documents — the MongoDB sibling of tableRowsQuery's page half. The exact
// total is cached separately (documentCountQuery) so paging through a large collection
// doesn't re-run count_documents on every page.
export function documentRowsQuery(args: DocumentRowsArgs) {
  const { connectionId, collection, pageSize, page } = args;
  return queryOptions({
    queryKey: qk.documentRows(args),
    staleTime: LOG_STALE_MS,
    queryFn: () =>
      runDocumentQuery(
        connectionId,
        { op: "find", collection, skip: page * pageSize, limit: pageSize },
        true,
      ),
  });
}

// A collection's exact document count, cached independent of page/pageSize so every page
// of the same collection shares one count_documents run.
export function documentCountQuery(connectionId: string, collection: string) {
  return queryOptions({
    queryKey: qk.documentCount(connectionId, collection),
    staleTime: LOG_STALE_MS,
    queryFn: async (): Promise<number | null> => {
      const countOut = await runDocumentQuery(connectionId, { op: "count", collection }, true);
      const count = (countOut.documents[0] as { count?: number } | undefined)?.count;
      return count == null ? null : Number(count);
    },
  });
}

// One page of table data plus its exact total. Both statements are issued together so a
// cached page always carries the row count that was true when it was read.
export function tableRowsQuery(args: TableRowsArgs) {
  const { connectionId, engine, table, filters, sort, pageSize, page } = args;
  return queryOptions({
    queryKey: qk.tableRows(args),
    staleTime: LOG_STALE_MS,
    queryFn: async (): Promise<TableRowsPage> => {
      const pageSql = buildPageQuery(engine, table, {
        filters,
        sort,
        limit: pageSize,
        offset: page * pageSize,
      });
      const [pageOut, countOut] = await Promise.all([
        runSql(connectionId, pageSql, true),
        runSql(connectionId, buildCountQuery(engine, table, filters), true),
      ]);
      const total = countOut.result?.rows?.[0]?.[0];
      return {
        result: pageOut.result ?? null,
        total: total == null ? null : Number(total),
      };
    },
  });
}
