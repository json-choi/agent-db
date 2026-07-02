// Query-history browser. Lists past statements for the selected connection (newest
// first); clicking a row pushes its SQL back into the editor via onLoadSql (App
// switches to the SQL tab and sets the draft).
import { useEffect, useState } from "react";
import { listHistory } from "../../ipc/commands";
import type { ConnectionProfile, HistoryEntry } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
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
  const [rows, setRows] = useState<HistoryEntry[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  function refresh() {
    setLoading(true);
    setErr(null);
    listHistory(connection.id)
      .then(setRows)
      .catch((e) => setErr(errMessage(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connection.id]);

  const shown = rows.slice(0, CAP);

  return (
    <div className="screen history">
      <div className="history-head">
        <span className="history-title">Query history</span>
        <button className="btn small" onClick={refresh} disabled={loading}>
          {loading ? "…" : "Refresh"}
        </button>
      </div>

      {err && <div className="error">{err}</div>}
      {!err && rows.length === 0 && (
        <div className="muted empty">
          No queries run against {connection.name || "this connection"} yet.
        </div>
      )}

      {shown.length > 0 && (
        <table className="history-table">
          <thead>
            <tr>
              <th>Executed</th>
              <th>Origin</th>
              <th>Kind</th>
              <th>Status</th>
              <th className="num">Rows</th>
              <th className="num">Duration</th>
              <th>SQL</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((h) => (
              <tr
                key={h.id}
                className="history-row"
                onClick={() => onLoadSql(h.sql)}
                title="Load this statement into the SQL editor"
              >
                <td className="nowrap muted">
                  {new Date(h.executedAt).toLocaleString()}
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

      {rows.length > CAP && (
        <div className="muted history-note">
          Showing latest {CAP} of {rows.length}.
        </div>
      )}
    </div>
  );
}
