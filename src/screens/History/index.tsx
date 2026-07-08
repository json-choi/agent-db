// Query-history browser. Lists past statements for the selected connection (newest
// first); clicking a row pushes its SQL back into the editor via onLoadSql (App
// switches to the SQL tab and sets the draft).
import { useCallback, useEffect, useMemo, useState } from "react";
import { listHistory } from "../../ipc/commands";
import type { ConnectionProfile, HistoryEntry } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { useToast } from "../../components/Toast";
import { relTime, fullTime } from "../../lib/relTime";
import { useI18n } from "../../lib/i18n";
import "./history.css";

const CAP = 200;

function duration(ms: number | null): string {
  if (ms == null) return "—";
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`;
}

function firstLine(sql: string): string {
  const line = sql.trim().split("\n")[0];
  return line.length > 120 ? `${line.slice(0, 120)}…` : line;
}

export default function History({
  connection,
  onLoadSql,
}: {
  connection: ConnectionProfile;
  onLoadSql: (sql: string) => void;
}) {
  const { t } = useI18n();
  const [rows, setRows] = useState<HistoryEntry[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [text, setText] = useState("");
  const [statusF, setStatusF] = useState("");
  const [originF, setOriginF] = useState("");
  const toast = useToast();

  const refresh = useCallback(() => {
    setLoading(true);
    setErr(null);
    listHistory(connection.id)
      .then(setRows)
      .catch((e) => setErr(errMessage(e)))
      .finally(() => setLoading(false));
  }, [connection.id]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Distinct values for the filter selects (from the loaded rows only).
  const statuses = useMemo(
    () => [...new Set(rows.map((h) => h.status))].sort(),
    [rows],
  );
  const origins = useMemo(
    () => [...new Set(rows.map((h) => h.origin))].sort(),
    [rows],
  );

  // Client-side filter over the already-loaded firehose; no backend round-trip.
  const filtered = rows.filter(
    (h) =>
      (!text || h.sql.toLowerCase().includes(text.toLowerCase())) &&
      (!statusF || h.status === statusF) &&
      (!originF || h.origin === originF),
  );
  const shown = filtered.slice(0, CAP);

  function load(sql: string) {
    onLoadSql(sql);
    toast(t("history.loaded"));
  }

  return (
    <div className="screen history">
      <div className="history-head">
        <span className="history-title">{t("history.title")}</span>
        <button className="btn small" onClick={refresh} disabled={loading}>
          {loading ? "..." : t("common.refresh")}
        </button>
      </div>

      {rows.length > 0 && (
        <div className="history-filters">
          <input
            className="history-filter-text"
            type="search"
            placeholder={t("history.filterSql")}
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
          <select value={statusF} onChange={(e) => setStatusF(e.target.value)}>
            <option value="">{t("history.allStatuses")}</option>
            {statuses.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
          <select value={originF} onChange={(e) => setOriginF(e.target.value)}>
            <option value="">{t("history.allOrigins")}</option>
            {origins.map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        </div>
      )}

      {err && <div className="error">{err}</div>}
      {!err && rows.length === 0 && (
        <div className="muted empty">
          {t("history.empty", {
            name: connection.name || t("app.thisConnection"),
          })}
        </div>
      )}

      {shown.length > 0 && (
        <table className="history-table">
          <thead>
            <tr>
              <th>{t("history.executed")}</th>
              <th>{t("history.origin")}</th>
              <th>{t("history.kind")}</th>
              <th>{t("history.status")}</th>
              <th className="num">{t("history.rows")}</th>
              <th className="num">{t("history.duration")}</th>
              <th>{t("history.sql")}</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((h) => (
              <tr
                key={h.id}
                className="history-row"
                role="button"
                tabIndex={0}
                onClick={() => load(h.sql)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    load(h.sql);
                  }
                }}
                title={t("history.loadTitle")}
              >
                <td className="nowrap muted" title={fullTime(h.executedAt)}>
                  {relTime(h.executedAt)}
                </td>
                <td>
                  <span className={`badge origin origin-${h.origin}`}>
                    {h.origin}
                  </span>
                </td>
                <td>
                  <span className="badge kind">{h.kind}</span>
                </td>
                <td>
                  <span className={`badge status status-${h.status}`}>
                    {h.status}
                  </span>
                </td>
                <td className="num">{h.rowCount ?? "—"}</td>
                <td className="num">{duration(h.durationMs)}</td>
                <td className="history-sql" title={h.sql}>
                  <code>{firstLine(h.sql)}</code>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {rows.length > 0 && filtered.length === 0 && (
        <div className="muted empty">{t("history.noMatches")}</div>
      )}

      {filtered.length > CAP && (
        <div className="muted history-note">
          {t("history.matching", { cap: CAP, count: filtered.length })}
        </div>
      )}
    </div>
  );
}
