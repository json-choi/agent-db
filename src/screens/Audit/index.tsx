// Append-only, hash-chained audit log viewer. Shows each transition and its chain
// link (prevHash → hash); the chain is verified server-side (real SHA-256 recompute).
import { useCallback, useEffect, useState } from "react";
import { listAudit, auditVerify } from "../../ipc/commands";
import type { AuditEntry } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { Icon } from "../../components/Icon";
import { relTime, fullTime } from "../../lib/relTime";
import { useI18n } from "../../lib/i18n";

function short(h: string | null): string {
  if (!h) return "∅";
  return h.length > 12 ? `${h.slice(0, 12)}…` : h;
}

type ChainVerdict = { ok: boolean; firstBadIndex: number | null };

export default function Audit({ connectionId }: { connectionId: string }) {
  const { t } = useI18n();
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [verdict, setVerdict] = useState<ChainVerdict | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(() => {
    setLoading(true);
    setMsg(null);
    // Fetch the log and its verification together so the verdict matches the rows shown.
    // Verification runs on the backend (rowid order + hash recompute) — a client link-only
    // check would mis-order same-timestamp rows and miss in-place field edits.
    Promise.all([listAudit(connectionId), auditVerify(connectionId)])
      .then(([es, v]) => {
        setEntries(es);
        setVerdict(v);
      })
      .catch((e) => setMsg(errMessage(e)))
      .finally(() => setLoading(false));
  }, [connectionId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // firstBadIndex is an insertion-order (oldest-first) position; entries are newest-first.
  const tamperedId =
    verdict && !verdict.ok && verdict.firstBadIndex != null
      ? entries[entries.length - 1 - verdict.firstBadIndex]?.id ?? null
      : null;
  const tamperedTs = tamperedId
    ? entries.find((e) => e.id === tamperedId)?.ts
    : null;

  return (
    <div className="screen audit">
      <div className="form-actions">
        <button className="btn small" onClick={refresh}>
          {t("common.refresh")}
        </button>
      </div>
      {msg && <div className="error">{msg}</div>}
      {loading && <div className="muted loading">{t("common.loading")}</div>}
      {!loading && entries.length === 0 && !msg && (
        <div className="muted empty">{t("audit.empty")}</div>
      )}
      {entries.length > 0 &&
        verdict &&
        (verdict.ok ? (
          <div className="chain-verdict ok">
            <Icon name="check" /> {t("audit.chainVerified", { count: entries.length })}
          </div>
        ) : (
          <div className="chain-verdict bad">
            <Icon name="alert" />{" "}
            {tamperedTs
              ? t("audit.chainBrokenAt", { time: relTime(tamperedTs) })
              : t("audit.chainBroken")}
          </div>
        ))}
      <ul className="audit-list">
        {entries.map((e) => (
          <li
            key={e.id}
            className={`audit-row${e.id === tamperedId ? " tampered" : ""}`}
          >
            <div className="audit-top">
              <span className={`badge action action-${e.action}`}>
                {e.action}
              </span>
              <span className="badge kind">{e.kind}</span>
              <span className="muted" title={fullTime(e.ts)}>
                {relTime(e.ts)}
              </span>
              {e.approvedBy && (
                <span className="muted">{t("audit.by", { name: e.approvedBy })}</span>
              )}
            </div>
            {e.agentPrompt && (
              <div className="audit-prompt muted" title={e.agentPrompt}>
                “{e.agentPrompt.length > 120
                  ? `${e.agentPrompt.slice(0, 120)}…`
                  : e.agentPrompt}”
              </div>
            )}
            <code className="audit-sql">{e.sql}</code>
            {e.error && <div className="error">{e.error}</div>}
            <div className="audit-chain muted">
              <span title={e.prevHash ?? ""}>
                {t("audit.prev", { hash: short(e.prevHash) })}
              </span>
              {" → "}
              <span title={e.hash}>{t("audit.hash", { hash: short(e.hash) })}</span>
              {e.affectedEstimate !== null && (
                <span> · {t("audit.rowsEstimate", { count: e.affectedEstimate })}</span>
              )}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}
