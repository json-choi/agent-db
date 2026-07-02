// L4 — the human approval gate, as UX. Given a connection + a SQL string, this card
// runs L1 classify + L3 preview, renders the full risk picture, and gates execution:
//   - read-only SELECTs may auto-run when the connection's autoRunReads is on;
//   - writes / DDL / privilege are ALWAYS hard-gated behind an explicit Approve,
//     and Approve is disabled unless the connection allows writes.
// Nothing here is trusted for safety — the Rust core re-enforces every gate (L2).

import { useEffect, useState } from "react";
import {
  classifySql,
  previewSql,
  runSql,
} from "../ipc/commands";
import type {
  Classification,
  Engine,
  ExecOutcome,
  PreviewReport,
  RiskLevel,
  SafetySettings,
} from "../ipc/types";
import { errMessage } from "../ipc/types";
import SqlViewer from "./SqlViewer";

const ENGINE_LABEL: Record<Engine, string> = {
  postgres: "PostgreSQL",
  mysql: "MySQL",
  sqlite: "SQLite",
};

function riskClass(risk: RiskLevel): string {
  return `badge risk-${risk}`;
}

export default function ApprovalCard({
  connectionId,
  engine,
  sql,
  safety,
  rationale,
  onExecuted,
  onReject,
}: {
  connectionId: string;
  engine: Engine;
  sql: string;
  safety: SafetySettings;
  rationale?: string;
  onExecuted: (outcome: ExecOutcome) => void;
  onReject?: () => void;
}) {
  const [cls, setCls] = useState<Classification | null>(null);
  const [preview, setPreview] = useState<PreviewReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [decided, setDecided] = useState<null | "approved" | "rejected">(null);

  // L1 + L3 whenever the statement changes.
  useEffect(() => {
    let alive = true;
    setCls(null);
    setPreview(null);
    setError(null);
    setDecided(null);
    if (!sql.trim()) return;
    (async () => {
      try {
        const c = await classifySql(connectionId, sql);
        if (!alive) return;
        setCls(c);
        const p = await previewSql(connectionId, sql);
        if (!alive) return;
        setPreview(p);
      } catch (e) {
        if (alive) setError(errMessage(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [connectionId, sql]);

  const isRead = cls?.kind === "read";
  const isWrite = !!cls && !isRead;
  const writesBlocked = isWrite && !safety.allowWrites;
  // Reads auto-run when the connection allows it — matching the backend gate
  // (`decide()` only applies require_approval to writes, never to reads).
  const canAutoRun = isRead && safety.autoRunReads && !!cls;

  async function execute(approved: boolean) {
    setBusy(true);
    setError(null);
    try {
      const outcome = await runSql(connectionId, sql, approved);
      setDecided("approved");
      onExecuted(outcome);
    } catch (e) {
      setError(errMessage(e));
    } finally {
      setBusy(false);
    }
  }

  // Auto-run reads (per settings) exactly once, after classification lands.
  useEffect(() => {
    if (canAutoRun && decided === null && !busy) {
      void execute(true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canAutoRun]);

  const previewN =
    preview?.exactRows ?? preview?.estimatedRows ?? null;

  return (
    <div className="card approval">
      <div className="approval-head">
        {cls ? (
          <>
            <span className="badge kind">{cls.kind.toUpperCase()}</span>
            <span className={riskClass(cls.risk)}>{cls.risk} risk</span>
            <span className="badge dialect">{ENGINE_LABEL[engine]}</span>
            {cls.noWhere && (
              <span className="badge nowhere">⚠ NO WHERE</span>
            )}
            {cls.statementCount > 1 && (
              <span className="badge nowhere">
                ⚠ {cls.statementCount} statements
              </span>
            )}
          </>
        ) : (
          <span className="muted">classifying…</span>
        )}
      </div>

      <SqlViewer value={sql} />

      {rationale && (
        <div className="restatement">
          <div className="label">Plain English</div>
          <p>{rationale}</p>
        </div>
      )}

      {cls && cls.tables.length > 0 && (
        <div className="tables">
          <span className="label">Target tables:</span>{" "}
          {cls.tables.map((t) => (
            <code key={t}>{t}</code>
          ))}
        </div>
      )}

      <div className="preview">
        <span className="label">Impact preview</span>
        {!preview ? (
          <span className="muted"> estimating…</span>
        ) : (
          <span>
            {" "}
            {preview.mode === "explain" && "EXPLAIN plan"}
            {preview.mode === "execRollback" && "executed + rolled back (exact)"}
            {preview.mode === "skipped" && "skipped (over threshold)"}
            {previewN !== null && (
              <>
                {" — "}
                <strong>{previewN.toLocaleString()}</strong> rows
              </>
            )}
            {preview.note && <em className="muted"> — {preview.note}</em>}
          </span>
        )}
      </div>

      {preview?.plan && (
        <details className="plan">
          <summary>Query plan</summary>
          <pre>{preview.plan}</pre>
        </details>
      )}

      {cls?.notes.map((n, i) => (
        <div key={i} className="note muted">
          • {n}
        </div>
      ))}

      {error && <div className="error">{error}</div>}

      {decided === "approved" ? (
        <div className="muted">Executed.</div>
      ) : decided === "rejected" ? (
        <div className="muted">Rejected.</div>
      ) : canAutoRun ? (
        <div className="muted">Read-only — auto-running…</div>
      ) : (
        <div className="approval-actions">
          {writesBlocked && (
            <div className="error">
              Writes are disabled for this connection (allow_writes = 0). Enable
              them in Safety to approve.
            </div>
          )}
          <button
            className="btn primary"
            disabled={busy || !cls || writesBlocked}
            onClick={() => execute(true)}
          >
            {isWrite ? "Approve & run write" : "Run"}
          </button>
          <button
            className="btn"
            disabled={busy}
            onClick={() => {
              setDecided("rejected");
              onReject?.();
            }}
          >
            Reject
          </button>
        </div>
      )}
    </div>
  );
}
