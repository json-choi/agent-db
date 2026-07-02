// Append-only, hash-chained audit log viewer. Shows each transition and its chain
// link (prevHash → hash) so tampering is visible.
import { useEffect, useState } from "react";
import { listAudit } from "../../ipc/commands";
import type { AuditEntry } from "../../ipc/types";
import { errMessage } from "../../ipc/types";

function short(h: string | null): string {
  if (!h) return "∅";
  return h.length > 12 ? `${h.slice(0, 12)}…` : h;
}

export default function Audit({ connectionId }: { connectionId: string }) {
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [msg, setMsg] = useState<string | null>(null);

  function refresh() {
    listAudit(connectionId)
      .then(setEntries)
      .catch((e) => setMsg(errMessage(e)));
  }

  useEffect(() => {
    setMsg(null);
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionId]);

  return (
    <div className="screen audit">
      <div className="form-actions">
        <button className="btn small" onClick={refresh}>
          Refresh
        </button>
      </div>
      {msg && <div className="error">{msg}</div>}
      {entries.length === 0 && !msg && (
        <div className="muted empty">No audit records yet.</div>
      )}
      <ul className="audit-list">
        {entries.map((e) => (
          <li key={e.id} className="audit-row">
            <div className="audit-top">
              <span className={`badge action action-${e.action}`}>
                {e.action}
              </span>
              <span className="badge kind">{e.kind}</span>
              <span className="muted">{new Date(e.ts).toLocaleString()}</span>
              {e.approvedBy && (
                <span className="muted">by {e.approvedBy}</span>
              )}
            </div>
            <code className="audit-sql">{e.sql}</code>
            {e.error && <div className="error">{e.error}</div>}
            <div className="audit-chain muted">
              <span title={e.prevHash ?? ""}>prev {short(e.prevHash)}</span>
              {" → "}
              <span title={e.hash}>hash {short(e.hash)}</span>
              {e.affectedEstimate !== null && (
                <span> · ~{e.affectedEstimate} rows</span>
              )}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}
