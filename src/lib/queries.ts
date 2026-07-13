// Query-key factory and shared query options for every cached backend read. Screens
// consume these via useQuery/useQueries so one fetch per (resource, connection) is shared
// app-wide: re-entering a tab repaints from cache and revalidates in the background.
// Invalidation lives in queryClient.tsx; nothing here fetches on its own.
import { queryOptions } from "@tanstack/react-query";
import {
  auditSnapshot,
  auditVerify,
  cancelQuery,
  getCatalog,
  getMonitoringStatus,
  listDashboards,
  listHistory,
  refreshCatalog,
  runDashboard,
  runSql,
} from "../ipc/commands";
import type { CatalogTable, Engine, QueryResult } from "../ipc/types";
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

export type TableRowsPage = { result: QueryResult | null; total: number | null };

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
  tableRows: (args: TableRowsArgs) =>
    [
      "tableRows",
      args.connectionId,
      tableKey(args.table),
      { filters: args.filters, sort: args.sort, pageSize: args.pageSize, page: args.page },
    ] as const,
};

export function catalogQuery(connectionId: string) {
  return queryOptions({
    queryKey: qk.catalog(connectionId),
    staleTime: CATALOG_STALE_MS,
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
