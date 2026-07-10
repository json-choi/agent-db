// Dependency-free dashboard renderer. It consumes a declarative chart spec,
// draws bounded SVG for visual scanning, and always exposes the raw data grid.
import type { DashboardVisualization, QueryResult } from "../ipc/types";
import {
  dashboardMapping,
  numericValue,
  resolvedDashboardKind,
} from "../lib/dashboardSpec";
import { useI18n } from "../lib/i18n";
import DataGrid from "./DataGrid";
import "./DashboardVisualization.css";

const VIEW_W = 800;
const VIEW_H = 280;
const PAD_L = 76;
const PAD_R = 20;
const PAD_T = 24;
const PAD_B = 48;
const PLOT_W = VIEW_W - PAD_L - PAD_R;
const PLOT_H = VIEW_H - PAD_T - PAD_B;

function compact(value: unknown): string {
  if (value == null) return "NULL";
  if (typeof value === "number") return value.toLocaleString();
  const text = String(value);
  return text.length > 14 ? `${text.slice(0, 13)}…` : text;
}

function metric(value: unknown): string {
  if (
    typeof value === "string" &&
    /^[-+]?\d+(\.\d+)?([eE][-+]?\d+)?$/.test(value.trim())
  ) {
    return value.trim();
  }
  const n = numericValue(value);
  if (n == null) return compact(value);
  return n.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

function seriesColor(index: number): string {
  if (index === 0) return "var(--ds-accent-text)";
  if (index === 1) {
    return "color-mix(in srgb, var(--ds-accent-text) 70%, var(--ds-text-muted))";
  }
  if (index === 2) {
    return "color-mix(in srgb, var(--ds-accent-text) 48%, var(--ds-text-muted))";
  }
  return "var(--ds-text-muted)";
}

function chartRows(result: QueryResult) {
  return result.rows.slice(0, 80);
}

function chartableColumns(result: QueryResult, columns: string[]) {
  const rows = chartRows(result);
  return columns.filter((column) => {
    const index = result.columns.indexOf(column);
    return index >= 0 && rows.some((row) => numericValue(row[index]) != null);
  });
}

function chartValues(result: QueryResult, yColumns: string[]) {
  return chartRows(result).flatMap((row) =>
    yColumns
      .map((column) => numericValue(row[result.columns.indexOf(column)]))
      .filter((value): value is number => value != null),
  );
}

function lineBounds(result: QueryResult, yColumns: string[]) {
  const values = chartValues(result, yColumns);
  if (values.length === 0) return { min: 0, max: 1 };
  const low = Math.min(...values);
  const high = Math.max(...values);
  const span = Math.max(Math.abs(high - low), Math.abs(high) * 0.04, 1);
  return { min: low - span * 0.08, max: high + span * 0.08 };
}

function barBounds(result: QueryResult, yColumns: string[]) {
  const values = chartValues(result, yColumns);
  if (values.length === 0) return { min: 0, max: 1 };
  let min = Math.min(0, ...values);
  let max = Math.max(0, ...values);
  if (min === max) {
    min -= 1;
    max += 1;
  }
  return { min, max };
}

function yAt(value: number, min: number, max: number) {
  return PAD_T + PLOT_H - ((value - min) / (max - min)) * PLOT_H;
}

function Axis({ min, max }: { min: number; max: number }) {
  return (
    <g className="dashboard-axis">
      {[0, 1, 2, 3, 4].map((step) => {
        const ratio = step / 4;
        const y = PAD_T + PLOT_H * ratio;
        const value = max - (max - min) * ratio;
        return (
          <g key={step}>
            <line x1={PAD_L} x2={VIEW_W - PAD_R} y1={y} y2={y} />
            <text x={PAD_L - 8} y={y + 4} textAnchor="end">
              {Number(value.toFixed(2)).toLocaleString()}
            </text>
          </g>
        );
      })}
    </g>
  );
}

function XLabels({ result, xColumn }: { result: QueryResult; xColumn: string | null }) {
  const rows = chartRows(result);
  if (!xColumn || rows.length === 0) return null;
  const xIndex = result.columns.indexOf(xColumn);
  const count = Math.min(6, rows.length);
  const indexes = Array.from({ length: count }, (_, i) =>
    Math.round((i * (rows.length - 1)) / Math.max(1, count - 1)),
  );
  return (
    <g className="dashboard-axis dashboard-x-axis">
      {[...new Set(indexes)].map((index) => {
        const x = PAD_L + (index / Math.max(1, rows.length - 1)) * PLOT_W;
        const textAnchor =
          index === 0 ? "start" : index === rows.length - 1 ? "end" : "middle";
        return (
          <text key={index} x={x} y={VIEW_H - 18} textAnchor={textAnchor}>
            {compact(rows[index]?.[xIndex])}
          </text>
        );
      })}
    </g>
  );
}

function Legend({ columns }: { columns: string[] }) {
  return (
    <div className="dashboard-legend" aria-hidden="true">
      {columns.map((column, index) => (
        <span key={column}>
          <i className={`dashboard-series-${Math.min(index, 3)}`} />
          {column}
        </span>
      ))}
    </div>
  );
}

function LineChart({
  result,
  visualization,
}: {
  result: QueryResult;
  visualization: DashboardVisualization;
}) {
  const { t } = useI18n();
  const rows = chartRows(result);
  const mapping = dashboardMapping(result, visualization);
  const xColumn = mapping.xColumn;
  const yColumns = chartableColumns(result, mapping.yColumns);
  const { min, max } = lineBounds(result, yColumns);
  return (
    <figure className="dashboard-chart" aria-label={t("dashboard.lineChartLabel")}>
      <Legend columns={yColumns} />
      <svg role="img" viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}>
        <title>{t("dashboard.lineChartLabel")}</title>
        <Axis min={min} max={max} />
        {yColumns.map((column, seriesIndex) => {
          const columnIndex = result.columns.indexOf(column);
          const points = rows
            .map((row, index) => {
              const value = numericValue(row[columnIndex]);
              if (value == null) return null;
              const x = PAD_L + (index / Math.max(1, rows.length - 1)) * PLOT_W;
              return `${x},${yAt(value, min, max)}`;
            })
            .filter((point): point is string => point != null)
            .join(" ");
          return (
            <polyline
              key={column}
              points={points}
              fill="none"
              stroke={seriesColor(seriesIndex)}
              strokeWidth={seriesIndex === 0 ? 3 : 2}
              strokeDasharray={seriesIndex > 1 ? "6 4" : undefined}
              vectorEffect="non-scaling-stroke"
            />
          );
        })}
        <XLabels result={result} xColumn={xColumn} />
      </svg>
    </figure>
  );
}

function BarChart({
  result,
  visualization,
}: {
  result: QueryResult;
  visualization: DashboardVisualization;
}) {
  const { t } = useI18n();
  const rows = chartRows(result).slice(0, 32);
  const mapping = dashboardMapping(
    { ...result, rows },
    visualization,
  );
  const xColumn = mapping.xColumn;
  const yColumns = chartableColumns({ ...result, rows }, mapping.yColumns);
  const { min, max } = barBounds({ ...result, rows }, yColumns);
  const zeroY = yAt(0, min, max);
  const groupWidth = PLOT_W / Math.max(1, rows.length);
  const barWidth = Math.max(2, (groupWidth * 0.72) / Math.max(1, yColumns.length));
  return (
    <figure className="dashboard-chart" aria-label={t("dashboard.barChartLabel")}>
      <Legend columns={yColumns} />
      <svg role="img" viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}>
        <title>{t("dashboard.barChartLabel")}</title>
        <Axis min={min} max={max} />
        {rows.flatMap((row, rowIndex) =>
          yColumns.map((column, seriesIndex) => {
            const value = numericValue(row[result.columns.indexOf(column)]);
            if (value == null) return null;
            const valueY = yAt(value, min, max);
            const x =
              PAD_L +
              rowIndex * groupWidth +
              groupWidth * 0.14 +
              seriesIndex * barWidth;
            return (
              <rect
                key={`${rowIndex}-${column}`}
                x={x}
                y={Math.min(valueY, zeroY)}
                width={barWidth}
                height={Math.max(1, Math.abs(zeroY - valueY))}
                fill={seriesColor(seriesIndex)}
                rx={1}
              />
            );
          }),
        )}
        <XLabels result={{ ...result, rows }} xColumn={xColumn} />
      </svg>
    </figure>
  );
}

function MetricView({
  result,
  visualization,
}: {
  result: QueryResult;
  visualization: DashboardVisualization;
}) {
  const mapping = dashboardMapping(result, visualization);
  const columns = mapping.yColumns.length > 0 ? mapping.yColumns : result.columns.slice(0, 4);
  const row = result.rows[0] ?? [];
  return (
    <div className="dashboard-metrics ds-card-grid">
      {columns.map((column) => (
        <article className="dashboard-metric card" key={column}>
          <span>{column}</span>
          <strong>{metric(row[result.columns.indexOf(column)])}</strong>
        </article>
      ))}
    </div>
  );
}

function TableFallback({ result }: { result: QueryResult }) {
  const { t } = useI18n();
  return (
    <div className="dashboard-table-fallback">
      <p className="muted">{t("dashboard.chartFallback")}</p>
      <DataGrid result={result} />
    </div>
  );
}

export default function DashboardVisualizationView({
  result,
  visualization,
}: {
  result: QueryResult;
  visualization: DashboardVisualization;
}) {
  const { t } = useI18n();
  if (result.rows.length === 0) return <div className="muted">{t("dashboard.noRows")}</div>;
  const kind = resolvedDashboardKind(result, visualization);
  const mapping = dashboardMapping(result, visualization);
  if (kind !== "table" && mapping.yColumns.length === 0) {
    return <TableFallback result={result} />;
  }
  if (
    (kind === "line" || kind === "bar") &&
    chartableColumns(result, mapping.yColumns).length === 0
  ) {
    return <TableFallback result={result} />;
  }
  if (kind === "table") return <DataGrid result={result} />;
  return (
    <>
      {kind === "metric" ? (
        <MetricView result={result} visualization={visualization} />
      ) : kind === "line" ? (
        <LineChart result={result} visualization={visualization} />
      ) : (
        <BarChart result={result} visualization={visualization} />
      )}
      <details className="dashboard-raw-data">
        <summary>{t("dashboard.rawData")}</summary>
        <DataGrid result={result} />
      </details>
    </>
  );
}
