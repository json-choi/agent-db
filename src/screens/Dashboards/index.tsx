// Persistent dashboard library for one database connection. Selecting a saved
// definition reruns its SQL through the existing read-only execution boundary.
import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { deleteDashboard } from "../../ipc/commands";
import type { ConnectionProfile, Dashboard, DashboardKind } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import ConfirmButton from "../../components/ConfirmButton";
import DashboardVisualizationView from "../../components/DashboardVisualization";
import { Icon } from "../../components/Icon";
import Skeleton from "../../components/Skeleton";
import { useToast } from "../../components/Toast";
import { dashboardRunQuery, dashboardsQuery, qk } from "../../lib/queries";
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
  const selected = useMemo(
    () => dashboards.find((dashboard) => dashboard.id === selectedId) ?? null,
    [dashboards, selectedId],
  );
  // Selecting a dashboard runs it. The result is cached per dashboard, so leaving the tab
  // and coming back repaints the chart instead of re-querying the database.
  const run = useQuery(dashboardRunQuery(selected?.id ?? null));

  const remove = useMutation({
    mutationFn: deleteDashboard,
    onSuccess: (_, removedId) => {
      queryClient.removeQueries({ queryKey: qk.dashboardRun(removedId) });
      void queryClient.invalidateQueries({ queryKey: qk.dashboards(connection.id) });
      setSelectedId((current) => (current === removedId ? null : current));
      toast(t("dashboard.deleted"));
    },
  });

  // Switching connections drops the selection; the focus effect below may immediately
  // restore one when the app navigated here to show a specific dashboard.
  useEffect(() => {
    setSelectedId(null);
    appliedFocusId.current = null;
  }, [connection.id]);

  useEffect(() => {
    if (!focusId || appliedFocusId.current === focusId) return;
    if (!dashboards.some((dashboard) => dashboard.id === focusId)) return;
    appliedFocusId.current = focusId;
    onFocusConsumed();
    setSelectedId(focusId);
  }, [dashboards, focusId, onFocusConsumed]);

  // Re-selecting the active dashboard is a rerun. Selecting a different one abandons the
  // in-flight read: cancelQueries aborts the query signal, which cancels it server-side.
  function execute(dashboard: Dashboard) {
    remove.reset(); // a failed delete must not keep reporting itself over the next run
    if (dashboard.id === selectedId) {
      void queryClient.invalidateQueries({ queryKey: qk.dashboardRun(dashboard.id) });
      return;
    }
    if (selectedId) void queryClient.cancelQueries({ queryKey: qk.dashboardRun(selectedId) });
    setSelectedId(dashboard.id);
  }

  const loading = list.isPending;
  const loadError = list.error ? errMessage(list.error) : null;
  const running = run.isFetching;
  const deleting = remove.isPending;
  const result = run.data ?? null;
  const runError = remove.error
    ? errMessage(remove.error)
    : run.error
      ? errMessage(run.error)
      : null;

  return (
    <div className="dashboards-screen screen">
      {loadError && (
        <div className="dashboard-error error" role="alert">
          {t("dashboard.loadFailed", { error: loadError })}
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
          <section className="dashboard-library" aria-label={t("dashboard.library")}>
            <div className="dashboard-library-head">
              <div>
                <strong>{t("dashboard.library")}</strong>
                <span className="muted">{t("dashboard.clickToRun")}</span>
              </div>
            </div>
            <ul className="dashboard-library-strip">
              {dashboards.map((dashboard) => (
                <li key={dashboard.id}>
                  <button
                    type="button"
                    disabled={running || deleting}
                    className={
                      dashboard.id === selectedId
                        ? "dashboard-library-card active"
                        : "dashboard-library-card"
                    }
                    aria-pressed={dashboard.id === selectedId}
                    onClick={() => void execute(dashboard)}
                  >
                    <span className="dashboard-library-title">
                      <Icon name="chart" />
                      <strong>{dashboard.title}</strong>
                    </span>
                    <span className="dashboard-library-meta">
                      {t(KIND_LABELS[dashboard.visualization.kind])}
                      <span className="ds-meta-dot" />
                      <time dateTime={dashboard.updatedAt}>{displayTime(dashboard.updatedAt)}</time>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </section>

          <section className="dashboard-canvas ds-panel">
            {!selected ? (
              <div className="dashboard-state dashboard-select-state">
                <span className="dashboard-state-icon"><Icon name="dashboard" /></span>
                <h3>{t("dashboard.selectTitle")}</h3>
                <p className="muted">{t("dashboard.selectBody")}</p>
              </div>
            ) : (
              <>
                <header className="dashboard-canvas-head">
                  <div className="dashboard-canvas-copy">
                    <div className="ds-title-line">
                      <h3>{selected.title}</h3>
                      <span className="badge kind">
                        {t(KIND_LABELS[selected.visualization.kind])}
                      </span>
                    </div>
                    {selected.description && <p className="muted">{selected.description}</p>}
                    <span className="dashboard-updated muted">
                      {t("dashboard.updatedAt", { time: displayTime(selected.updatedAt) })}
                    </span>
                  </div>
                  <div className="ds-command-group">
                    <button
                      className="btn small"
                      disabled={running || deleting}
                      onClick={() => void execute(selected)}
                    >
                      <Icon name="refresh" />
                      {t("dashboard.refresh")}
                    </button>
                    <ConfirmButton
                      className="btn danger small"
                      disabled={running || deleting}
                      confirmLabel={t("dashboard.deleteConfirm")}
                      onConfirm={() => remove.mutate(selected.id)}
                    >
                      <Icon name="trash" />
                      {t("common.delete")}
                    </ConfirmButton>
                  </div>
                </header>

                <details className="dashboard-query">
                  <summary>{t("dashboard.sql")}</summary>
                  <code>{selected.sql}</code>
                </details>

                {runError && <div className="error" role="alert">{runError}</div>}
                {running && !result ? (
                  <div className="dashboard-state dashboard-running" aria-busy="true">
                    <span className="loading">{t("dashboard.running")}</span>
                  </div>
                ) : result ? (
                  <div className="dashboard-result">
                    <div className="dashboard-result-meta muted">
                      {t("dashboard.resultMeta", {
                        count: result.rowCount,
                        duration: result.durationMs,
                      })}
                      {result.truncated ? ` · ${t("dashboard.truncated")}` : ""}
                    </div>
                    <DashboardVisualizationView
                      result={result}
                      visualization={selected.visualization}
                    />
                  </div>
                ) : !runError ? (
                  <div className="dashboard-state dashboard-running">
                    <span className="muted">{t("dashboard.clickToRun")}</span>
                  </div>
                ) : null}
              </>
            )}
          </section>
        </>
      )}
    </div>
  );
}
