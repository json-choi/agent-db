// Persistent dashboard library for one database connection. Selecting a saved
// definition reruns its SQL through the existing read-only execution boundary.
import { useEffect, useMemo, useRef, useState } from "react";
import {
  cancelQuery,
  deleteDashboard,
  listDashboards,
  runDashboard,
} from "../../ipc/commands";
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
import { useToast } from "../../components/Toast";
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
  const [dashboards, setDashboards] = useState<Dashboard[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [runError, setRunError] = useState<string | null>(null);
  const listRequest = useRef(0);
  const runRequest = useRef(0);
  const selectedIdRef = useRef<string | null>(null);
  const appliedFocusId = useRef<string | null>(null);
  const activeQueryId = useRef<string | null>(null);

  const selected = useMemo(
    () => dashboards.find((dashboard) => dashboard.id === selectedId) ?? null,
    [dashboards, selectedId],
  );

  async function execute(dashboard: Dashboard) {
    const request = ++runRequest.current;
    if (activeQueryId.current) void cancelQuery(activeQueryId.current);
    const queryId = window.crypto.randomUUID();
    activeQueryId.current = queryId;
    selectedIdRef.current = dashboard.id;
    setSelectedId(dashboard.id);
    setResult(null);
    setRunError(null);
    setRunning(true);
    try {
      const nextResult = await runDashboard(dashboard.id, queryId);
      if (request !== runRequest.current) return;
      setResult(nextResult);
    } catch (error) {
      if (request === runRequest.current) setRunError(errMessage(error));
    } finally {
      if (activeQueryId.current === queryId) activeQueryId.current = null;
      if (request === runRequest.current) setRunning(false);
    }
  }

  useEffect(() => {
    const request = ++listRequest.current;
    ++runRequest.current;
    setLoading(true);
    setLoadError(null);
    setRunError(null);
    setResult(null);
    selectedIdRef.current = null;
    setSelectedId(null);
    appliedFocusId.current = null;

    listDashboards(connection.id)
      .then((items) => {
        if (request !== listRequest.current) return;
        setDashboards(items);
      })
      .catch((error) => {
        if (request === listRequest.current) setLoadError(errMessage(error));
      })
      .finally(() => {
        if (request === listRequest.current) setLoading(false);
      });

    return () => {
      const active = activeQueryId.current;
      activeQueryId.current = null;
      if (active) void cancelQuery(active);
      ++listRequest.current;
      ++runRequest.current;
    };
  }, [connection.id]);

  useEffect(() => {
    if (!focusId || appliedFocusId.current === focusId) return;
    const focused = dashboards.find((dashboard) => dashboard.id === focusId);
    if (!focused) return;
    appliedFocusId.current = focusId;
    onFocusConsumed();
    void execute(focused);
  }, [dashboards, focusId, onFocusConsumed]);

  async function removeSelected() {
    if (!selected || deleting) return;
    const deletingId = selected.id;
    setDeleting(true);
    setRunError(null);
    try {
      await deleteDashboard(deletingId);
      setDashboards((items) => items.filter((item) => item.id !== deletingId));
      if (selectedIdRef.current === deletingId) {
        selectedIdRef.current = null;
        setSelectedId(null);
        setResult(null);
      }
      toast(t("dashboard.deleted"));
    } catch (error) {
      setRunError(errMessage(error));
    } finally {
      setDeleting(false);
    }
  }

  return (
    <div className="dashboards-screen screen">
      <header className="dashboard-page-head">
        <div>
          <div className="ds-title-line">
            <Icon name="dashboard" />
            <h2>{t("dashboard.workspace")}</h2>
          </div>
          <p className="muted">{t("dashboard.workspaceBody")}</p>
        </div>
        <span className="badge kind">
          {t("dashboard.savedCount", { count: dashboards.length })}
        </span>
      </header>

      {loadError && (
        <div className="dashboard-error error" role="alert">
          {t("dashboard.loadFailed", { error: loadError })}
        </div>
      )}

      {loading ? (
        <section className="dashboard-state ds-panel" aria-busy="true">
          <span className="loading">{t("dashboard.loading")}</span>
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
                      onConfirm={() => void removeSelected()}
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
                {running ? (
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
