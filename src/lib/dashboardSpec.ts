// Pure inference helpers for mapping arbitrary SQL results onto the versioned
// dashboard visualization contract. Numeric strings stay lossless until render.
import type {
  DashboardKind,
  DashboardVisualization,
  QueryResult,
} from "../ipc/types";

export interface DashboardMapping {
  xColumn: string | null;
  yColumns: string[];
}

export function numericValue(value: unknown): number | null {
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value !== "string" || value.trim() === "") return null;
  const normalized = value.trim();
  if (!/^[-+]?\d+(\.\d+)?([eE][-+]?\d+)?$/.test(normalized)) return null;
  const parsed = Number(normalized);
  // Exact BIGINT/DECIMAL strings intentionally arrive as strings. Chart coordinates
  // may be approximate, but never coerce magnitudes that exceed JS's safe range.
  if (Math.abs(parsed) > Number.MAX_SAFE_INTEGER) return null;
  return Number.isFinite(parsed) ? parsed : null;
}

function numericLike(value: unknown): boolean {
  if (typeof value === "number") return Number.isFinite(value);
  return (
    typeof value === "string" &&
    /^[-+]?\d+(\.\d+)?([eE][-+]?\d+)?$/.test(value.trim())
  );
}

function numericColumn(result: QueryResult, index: number): boolean {
  const values = result.rows.map((row) => row[index]).filter((value) => value != null);
  return values.length > 0 && values.every(numericLike);
}

function temporalColumn(result: QueryResult, column: string): boolean {
  if (/date|time|day|week|month|year|created|updated/i.test(column)) return true;
  const index = result.columns.indexOf(column);
  if (index < 0) return false;
  const values = result.rows
    .map((row) => row[index])
    .filter((value): value is string => typeof value === "string" && value.trim() !== "")
    .slice(0, 12);
  return (
    values.length > 0 &&
    values.every((value) => !/^[-+]?\d+(\.\d+)?$/.test(value) && !Number.isNaN(Date.parse(value)))
  );
}

export function inferDashboardMapping(result: QueryResult): DashboardMapping {
  const numeric = result.columns.filter((_, index) => numericColumn(result, index));
  if (result.rows.length === 1) {
    return { xColumn: null, yColumns: numeric.slice(0, 4) };
  }
  const category = result.columns.find((column) => !numeric.includes(column));
  const xColumn = category ?? (numeric.length > 1 ? result.columns[0] ?? null : null);
  const yColumns = numeric.filter((column) => column !== xColumn).slice(0, 4);
  return { xColumn, yColumns };
}

export function dashboardMapping(
  result: QueryResult,
  visualization: DashboardVisualization,
): DashboardMapping {
  const inferred = inferDashboardMapping(result);
  const xColumn = result.columns.includes(visualization.xColumn ?? "")
    ? visualization.xColumn
    : inferred.xColumn;
  const yColumns = visualization.yColumns.filter((column) => {
    const index = result.columns.indexOf(column);
    return index >= 0 && numericColumn(result, index);
  });
  return { xColumn, yColumns: yColumns.length > 0 ? yColumns : inferred.yColumns };
}

export function resolvedDashboardKind(
  result: QueryResult,
  visualization: DashboardVisualization,
): Exclude<DashboardKind, "auto"> {
  if (visualization.kind !== "auto") return visualization.kind;
  const mapping = dashboardMapping(result, visualization);
  if (result.rows.length === 1 && mapping.yColumns.length > 0) return "metric";
  if (mapping.xColumn && mapping.yColumns.length > 0) {
    return temporalColumn(result, mapping.xColumn) ? "line" : "bar";
  }
  return "table";
}
