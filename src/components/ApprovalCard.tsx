// L4 — the human approval gate, as UX. Given a connection + a SQL string, this card
// runs L1 classify + L3 preview, renders the full risk picture, and gates execution:
//   - read-only SELECTs may auto-run when the connection's autoRunReads is on;
//   - writes / DDL / privilege are ALWAYS hard-gated behind an explicit Approve,
//     and Approve is disabled unless the connection allows writes.
// Nothing here is trusted for safety — the Rust core re-enforces every gate (L2).

import { useEffect, useRef, useState } from "react";
import {
  cancelQuery,
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
import { Icon } from "./Icon";
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
  const [cancelled, setCancelled] = useState(false);
  // The in-flight query id, so Cancel can signal it. Held in a ref (not state) since
  // execute() reads it synchronously and it never needs to re-render. `cancelledRef`
  // mirrors the flag so execute()'s catch sees it without a stale closure.
  const queryId = useRef<string | null>(null);
  const cancelledRef = useRef(false);
  // Elapsed seconds while a query runs, so a slow query reads differently from a hung one.
  const [elapsed, setElapsed] = useState(0);

  // L1 + L3 whenever the statement changes.
  useEffect(() => {
    let alive = true;
    setCls(null);
    setPreview(null);
    setError(null);
    setDecided(null);
    setCancelled(false);
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
    const id = crypto.randomUUID();
    queryId.current = id;
    cancelledRef.current = false;
    setBusy(true);
    setError(null);
    setCancelled(false);
    try {
      const outcome = await runSql(connectionId, sql, approved, id);
      setDecided("approved");
      onExecuted(outcome);
    } catch (e) {
      // A cancelled query fails on the backend — show it as a benign note, not an error.
      if (cancelledRef.current) setCancelled(true);
      else setError(errMessage(e));
    } finally {
      queryId.current = null;
      setBusy(false);
    }
  }

  function cancel() {
    if (queryId.current) {
      cancelledRef.current = true;
      void cancelQuery(queryId.current);
    }
  }

  // Tick the elapsed counter while busy; reset+clear when done or unmounted.
  useEffect(() => {
    if (!busy) {
      setElapsed(0);
      return;
    }
    const t = setInterval(() => setElapsed((s) => s + 1), 1000);
    return () => clearInterval(t);
  }, [busy]);

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
        ) : isWrite && !writesBlocked && previewN === null ? (
          // A runnable write with NO row estimate (skipped over threshold, or an EXPLAIN
          // that yielded no count) means approving a destructive statement blind — surface
          // it. Not for writes-disabled (can't run) or reads (a null estimate is benign).
          <span className="impact-warn">
            {" "}
            <Icon name="alert" /> Impact could not be estimated — affected row
            count unknown
            {preview.note && <em className="muted"> — {preview.note}</em>}
          </span>
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
      {/* Additive, not a terminal branch — the action buttons below stay reachable so a
          cancelled query can simply be run again. */}
      {cancelled && <div className="muted">Query cancelled.</div>}

      {decided === "approved" ? (
        <div className="muted">Executed.</div>
      ) : decided === "rejected" ? (
        // Not a dead-end: keep the statement visible above and let the user undo the
        // rejection to approve it, rather than forcing a re-issue.
        <div className="approval-actions">
          <span className="muted">Rejected.</span>
          <button className="btn" onClick={() => setDecided(null)}>
            Reconsider
          </button>
        </div>
      ) : busy ? (
        <div className="approval-actions">
          <span className="muted">
            {canAutoRun ? "Read-only — running…" : "Running…"} {elapsed}s
          </span>
          <button className="btn" onClick={cancel}>
            Cancel
          </button>
        </div>
      ) : canAutoRun && !cancelled ? (
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
