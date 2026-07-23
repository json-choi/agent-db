// Connection-scoped dashboard canvas. Every saved Agent query becomes one live tile,
// mirroring Chat2DB's at-a-glance report surface while retaining DopeDB's read-only
// execution boundary and explicit per-tile refresh/delete controls.
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { deleteDashboard } from "../../ipc/commands";
import type {
  ConnectionProfile,
  Dashboard,
  DashboardKind,
  QueryResult,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import ConfirmButton from "../../components/ConfirmButton";
import DashboardVisualizationView from "../../components/DashboardVisualization";
import { Icon } from "../../components/Icon";
import Skeleton from "../../components/Skeleton";
import { useToast } from "../../components/Toast";
import { dashboardTileRunQueries, dashboardsQuery, qk } from "../../lib/queries";
import { useI18n, type I18nKey } from "../../lib/i18n";
import "./dashboards.css";

const KIND_LABELS: Record<DashboardKind, I18nKey> = {
  auto: "dashboard.kindAuto",
  bar: "dashboard.kindBar",
  line: "dashboard.kindLine",
  metric: "dashboard.kindMetric",
  table: "dashboard.kindTable",
};

function displayTime(value: string) {
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

export function DashboardSidebar({
  connections,
  selectedId,
  focusId,
  onSelectConnection,
  onFocus,
  workspaceAccount,
  workspaceHeader,
}: {
  connections: ConnectionProfile[];
  selectedId: string | null;
  focusId: string | null;
  onSelectConnection: (id: string) => void;
  onFocus: (id: string) => void;
  workspaceAccount?: ReactNode;
  workspaceHeader?: ReactNode;
}) {
  const { t } = useI18n();
  const selected = connections.find((connection) => connection.id === selectedId) ?? null;
  const list = useQuery({
    ...dashboardsQuery(selectedId ?? "__no_connection__"),
    enabled: selectedId !== null,
  });
  const dashboards = list.data ?? [];

  return (
    <aside className="sidebar dashboard-sidebar">
      {workspaceHeader}
      <div className="dashboard-sidebar-body">
        <label className="dashboard-connection-picker">
          <span>{t("app.thisConnection")}</span>
          <select
            value={selectedId ?? ""}
            onChange={(event) => onSelectConnection(event.target.value)}
          >
            <option value="" disabled>
              {t("settings.selectConnectionTitle")}
            </option>
            {connections.map((connection) => (
              <option key={connection.id} value={connection.id}>
                {connection.name || t("app.unnamed")}
              </option>
            ))}
          </select>
        </label>

        <div className="dashboard-sidebar-heading">
          <strong>{t("dashboard.library")}</strong>
          <span className="muted">{dashboards.length}</span>
        </div>

        <div className="dashboard-sidebar-list">
          {!selected ? (
            <p className="muted">{t("settings.selectConnectionTitle")}</p>
          ) : list.isPending ? (
            <Skeleton lines={4} />
          ) : list.error ? (
            <p className="error">{errMessage(list.error)}</p>
          ) : dashboards.length === 0 ? (
            <p className="muted">{t("dashboard.emptyTitle")}</p>
          ) : (
            dashboards.map((dashboard) => (
              <button
                type="button"
                key={dashboard.id}
                className={`dashboard-sidebar-item${focusId === dashboard.id ? " active" : ""}`}
                onClick={() => onFocus(dashboard.id)}
                title={dashboard.title}
              >
                <Icon name="chart" />
                <span>
                  <strong>{dashboard.title}</strong>
                  <small>{t(KIND_LABELS[dashboard.visualization.kind])}</small>
                </span>
              </button>
            ))
          )}
        </div>
      </div>
      {workspaceAccount ? (
        <div className="sidebar-foot ds-control-row">{workspaceAccount}</div>
      ) : null}
    </aside>
  );
}

function DashboardTile({
  dashboard,
  result,
  running,
  error,
  deleting,
  selected,
  onRefresh,
  onDelete,
}: {
  dashboard: Dashboard;
  result: QueryResult | null;
  running: boolean;
  error: string | null;
  deleting: boolean;
  selected: boolean;
  onRefresh: () => void;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  return (
    <article
      id={`dashboard-${dashboard.id}`}
      className={`dashboard-tile kind-${dashboard.visualization.kind}${selected ? " active" : ""}`}
      tabIndex={-1}
    >
      <header className="dashboard-tile-head">
        <div className="dashboard-tile-copy">
          <div className="ds-title-line">
            <Icon name="chart" />
            <strong>{dashboard.title}</strong>
            <span className="badge kind">
              {t(KIND_LABELS[dashboard.visualization.kind])}
            </span>
          </div>
          {dashboard.description && <p className="muted">{dashboard.description}</p>}
        </div>
        <div className="ds-control-row">
          <button
            type="button"
            className="btn small"
            disabled={running || deleting}
            onClick={onRefresh}
            title={t(selected ? "dashboard.refresh" : "dashboard.clickToRun")}
            aria-label={t(selected ? "dashboard.refresh" : "dashboard.clickToRun")}
          >
            <Icon name={selected ? "refresh" : "play"} />
          </button>
          <ConfirmButton
            className="btn danger small"
            disabled={running || deleting}
            confirmLabel={t("dashboard.deleteConfirm")}
            onConfirm={onDelete}
          >
            <Icon name="trash" />
          </ConfirmButton>
        </div>
      </header>

      <div className="dashboard-tile-meta muted">
        <time dateTime={dashboard.updatedAt}>{displayTime(dashboard.updatedAt)}</time>
        {result && (
          <>
            <span className="ds-meta-dot" />
            <span>
              {t("dashboard.resultMeta", {
                count: result.rowCount,
                duration: result.durationMs,
              })}
            </span>
          </>
        )}
      </div>

      {error ? (
        <div className="error" role="alert">{error}</div>
      ) : running && !result ? (
        <div className="dashboard-tile-state" aria-busy="true">
          <span className="loading">{t("dashboard.running")}</span>
        </div>
      ) : result ? (
        <DashboardVisualizationView
          compact
          result={result}
          visualization={dashboard.visualization}
        />
      ) : (
        <div className="dashboard-tile-state muted">{t("dashboard.clickToRun")}</div>
      )}

      <details className="dashboard-query">
        <summary>{t("dashboard.sql")}</summary>
        <code>{dashboard.sql}</code>
      </details>
    </article>
  );
}

export default function Dashboards({
  connection,
  focusId,
  onFocusConsumed,
  onOpenAgent,
}: {
  connection: ConnectionProfile;
  focusId: string | null;
  onFocusConsumed: () => void;
  onOpenAgent: () => void;
}) {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const appliedFocusId = useRef<string | null>(null);

  const list = useQuery(dashboardsQuery(connection.id));
  const dashboards = useMemo(() => list.data ?? [], [list.data]);
  const runs = useQueries({
    queries: dashboardTileRunQueries(
      dashboards.map((dashboard) => dashboard.id),
      selectedId,
    ),
  });

  const remove = useMutation({
    mutationFn: deleteDashboard,
    onSuccess: (_, removedId) => {
      queryClient.removeQueries({ queryKey: qk.dashboardRun(removedId) });
      void queryClient.invalidateQueries({ queryKey: qk.dashboards(connection.id) });
      setSelectedId((current) => (current === removedId ? null : current));
      toast(t("dashboard.deleted"));
    },
  });

  useEffect(() => {
    setSelectedId(null);
    appliedFocusId.current = null;
  }, [connection.id]);

  useEffect(() => {
    if (!focusId || appliedFocusId.current === focusId) return;
    if (!dashboards.some((dashboard) => dashboard.id === focusId)) return;
    appliedFocusId.current = focusId;
    onFocusConsumed();
    if (selectedId && selectedId !== focusId) {
      void queryClient.cancelQueries({ queryKey: qk.dashboardRun(selectedId) });
    }
    setSelectedId(focusId);
    window.requestAnimationFrame(() => {
      const tile = document.getElementById(`dashboard-${focusId}`);
      tile?.scrollIntoView({ block: "center", behavior: "smooth" });
      tile?.focus({ preventScroll: true });
    });
  }, [dashboards, focusId, onFocusConsumed, queryClient, selectedId]);

  function execute(dashboard: Dashboard) {
    remove.reset();
    if (dashboard.id === selectedId) {
      void queryClient.invalidateQueries({ queryKey: qk.dashboardRun(dashboard.id) });
      return;
    }
    if (selectedId) {
      void queryClient.cancelQueries({ queryKey: qk.dashboardRun(selectedId) });
    }
    setSelectedId(dashboard.id);
  }

  const loading = list.isPending;
  const loadError = list.error ? errMessage(list.error) : null;
  const deleteError = remove.error ? errMessage(remove.error) : null;

  return (
    <div className="dashboards-screen screen">
      {loadError && (
        <div className="dashboard-error error" role="alert">
          {t("dashboard.loadFailed", { error: loadError })}
        </div>
      )}
      {deleteError && (
        <div className="dashboard-error error" role="alert">
          {deleteError}
        </div>
      )}

      {loading ? (
        <section className="dashboard-state ds-panel">
          <Skeleton lines={3} />
        </section>
      ) : loadError ? null : dashboards.length === 0 ? (
        <section className="dashboard-state dashboard-empty ds-panel">
          <span className="dashboard-state-icon"><Icon name="chart" /></span>
          <h3>{t("dashboard.emptyTitle")}</h3>
          <p className="muted">{t("dashboard.emptyBody")}</p>
          <button className="btn primary" onClick={onOpenAgent}>
            <Icon name="play" />
            {t("dashboard.openAgent")}
          </button>
        </section>
      ) : (
        <>
          <header className="dashboard-overview-head">
            <div>
              <strong>{t("dashboard.library")}</strong>
              <span className="muted">{dashboards.length}</span>
            </div>
            <button className="btn small" onClick={onOpenAgent}>
              <Icon name="play" />
              {t("dashboard.openAgent")}
            </button>
          </header>
          <section className="dashboard-grid" aria-label={t("dashboard.library")}>
            {dashboards.map((dashboard, index) => {
              const run = runs[index];
              return (
                <DashboardTile
                  key={dashboard.id}
                  dashboard={dashboard}
                  result={run?.data ?? null}
                  running={run?.isFetching ?? false}
                  error={run?.error ? errMessage(run.error) : null}
                  deleting={remove.isPending && remove.variables === dashboard.id}
                  selected={dashboard.id === selectedId}
                  onRefresh={() => execute(dashboard)}
                  onDelete={() => remove.mutate(dashboard.id)}
                />
              );
            })}
          </section>
        </>
      )}
    </div>
  );
}
