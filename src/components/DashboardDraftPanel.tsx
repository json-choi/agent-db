// Inline, non-modal form that turns a live MCP query result into a persistent
// dashboard definition without storing result rows or generated HTML.
import { useState, type FormEvent } from "react";
import { saveDashboard } from "../ipc/commands";
import type { Dashboard, DashboardKind, QueryResult } from "../ipc/types";
import { errMessage } from "../ipc/types";
import { inferDashboardMapping } from "../lib/dashboardSpec";
import { useI18n, type I18nKey } from "../lib/i18n";
import { Icon } from "./Icon";
import "./DashboardDraftPanel.css";

const KINDS: DashboardKind[] = ["auto", "metric", "line", "bar", "table"];
const KIND_LABELS: Record<DashboardKind, I18nKey> = {
  auto: "dashboard.kindAuto",
  metric: "dashboard.kindMetric",
  line: "dashboard.kindLine",
  bar: "dashboard.kindBar",
  table: "dashboard.kindTable",
};

export default function DashboardDraftPanel({
  connectionId,
  sql,
  result,
  onSaved,
  onCancel,
}: {
  connectionId: string;
  sql: string;
  result: QueryResult;
  onSaved: (dashboard: Dashboard) => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const [title, setTitle] = useState(() => t("dashboard.defaultTitle"));
  const [kind, setKind] = useState<DashboardKind>("auto");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const normalizedTitle = title.trim();
    if (!normalizedTitle || saving) return;
    setSaving(true);
    setError(null);
    try {
      const mapping = inferDashboardMapping(result);
      const dashboard = await saveDashboard({
        connectionId,
        title: normalizedTitle,
        description: "",
        sql,
        visualization: {
          version: 1,
          kind,
          xColumn: mapping.xColumn,
          yColumns: mapping.yColumns,
        },
      });
      onSaved(dashboard);
    } catch (e) {
      setError(errMessage(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <form className="dashboard-save-panel ds-panel" onSubmit={submit}>
      <div className="dashboard-save-head">
        <div className="ds-title-line">
          <Icon name="dashboard" />
          <strong>{t("dashboard.saveResult")}</strong>
        </div>
        <button
          type="button"
          className="btn small"
          onClick={onCancel}
          aria-label={t("common.close")}
          title={t("common.close")}
        >
          <Icon name="close" />
        </button>
      </div>
      <div className="dashboard-save-fields">
        <label>
          <span>{t("dashboard.name")}</span>
          <input
            autoFocus
            required
            value={title}
            maxLength={120}
            onChange={(event) => setTitle(event.target.value)}
          />
        </label>
        <label>
          <span>{t("dashboard.chartType")}</span>
          <select
            value={kind}
            onChange={(event) => setKind(event.target.value as DashboardKind)}
          >
            {KINDS.map((value) => (
              <option key={value} value={value}>
                {t(KIND_LABELS[value])}
              </option>
            ))}
          </select>
        </label>
      </div>
      <p className="muted dashboard-save-note">{t("dashboard.saveNote")}</p>
      {error && <div className="error" role="alert">{error}</div>}
      <div className="ds-action-row ds-control-row">
        <button type="button" className="btn" disabled={saving} onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button className="btn primary" disabled={!title.trim() || saving}>
          {saving ? t("dashboard.saving") : t("common.save")}
        </button>
      </div>
    </form>
  );
}
