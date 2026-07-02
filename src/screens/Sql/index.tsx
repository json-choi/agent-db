// Manual SQL console. Editable CodeMirror. A single statement goes through the exact
// same ApprovalCard safety gate as agent output. A pasted multi-statement script (>1)
// switches to script mode: an up-front confirm panel (list + one checkbox + Execute),
// then per-statement results stacked below. ⌘↩ runs the current draft. The draft lives
// in App so it survives switching tabs.
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ConnectionProfile,
  Engine,
  ExecOutcome,
  SafetySettings,
  ScriptOutcome,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { cancelQuery, classifySql, previewSql, runScript } from "../../ipc/commands";
import type { PreviewReport } from "../../ipc/types";
import { splitStatements } from "../../lib/sqlStatements";
import ApprovalCard from "../../components/ApprovalCard";
import SqlViewer from "../../components/SqlViewer";
import DataGrid from "../../components/DataGrid";
import ResultToolbar from "../../components/ResultToolbar";
import { stamp } from "../../lib/export";
import "./sql.css";

const STEP = 200;

const ENGINE_LABEL: Record<Engine, string> = {
  postgres: "PostgreSQL",
  mysql: "MySQL",
  sqlite: "SQLite",
};

interface Run {
  sql: string;
  outcome: ExecOutcome;
  at: string;
}

export default function Sql({
  connection,
  safety,
  draft,
  setDraft,
}: {
  connection: ConnectionProfile;
  safety: SafetySettings;
  draft: string;
  setDraft: (s: string) => void;
}) {
  const draftStatements = useMemo(() => splitStatements(draft), [draft]);
  const draftIsScript = draftStatements.length > 1;

  // The armed target drives which gate renders. ⌘↩ with a selection runs just the
  // selection, so the single/script decision is made from the target, not the draft.
  const [target, setTarget] = useState<string | null>(null);
  const targetStatements = useMemo(
    () => (target ? splitStatements(target) : []),
    [target],
  );
  const isScript = targetStatements.length > 1;

  // Single-statement flow. runSeq feeds the gate keys so re-running the identical SQL
  // remounts a fresh card (otherwise key={sql} keeps the old decided/cancelled state).
  const [prepared, setPrepared] = useState<string | null>(null);
  const [run, setRun] = useState<Run | null>(null);
  const [limit, setLimit] = useState(STEP);
  const [runSeq, setRunSeq] = useState(0);

  // Script flow.
  const [armed, setArmed] = useState<string | null>(null);
  const [scriptOut, setScriptOut] = useState<{ outcome: ScriptOutcome; at: string } | null>(null);

  // EXPLAIN plan (read-only preview) shown above the results, independent of execution.
  const [plan, setPlan] = useState<PreviewReport | null>(null);
  const [planErr, setPlanErr] = useState<string | null>(null);

  // ⌘↩ and the Run button both just ARM the run — the gate (ApprovalCard for a single
  // statement, the confirm panel for a script) is never bypassed. A ⌘↩ selection runs
  // alone; the Run button always runs the whole draft.
  function armRun(selectedSql?: string) {
    const sql = selectedSql?.trim() || draft;
    if (!sql.trim()) return;
    setTarget(sql);
    setRunSeq((s) => s + 1);
    if (splitStatements(sql).length > 1) {
      setArmed(sql);
      setScriptOut(null);
    } else {
      setPrepared(sql);
      setRun(null);
    }
  }

  async function explain() {
    if (!draft.trim() || draftIsScript) return;
    setPlanErr(null);
    try {
      // Reads only: preview_sql on a write does an execute+rollback (locks, triggers) —
      // that impact preview belongs to the Run approval card, not a casual Explain.
      const cls = await classifySql(connection.id, draft);
      if (cls.kind !== "read") {
        setPlan(null);
        setPlanErr("Explain is for read statements — Run shows a write's impact preview instead.");
        return;
      }
      setPlan(await previewSql(connection.id, draft));
    } catch (e) {
      setPlanErr(errMessage(e));
      setPlan(null);
    }
  }

  // A plan describes the draft it was generated from — invalidate it on edit.
  useEffect(() => {
    setPlan(null);
    setPlanErr(null);
  }, [draft]);

  return (
    <div className="screen sqlconsole">
      <div className="editor-box">
        <SqlViewer value={draft} editable onChange={setDraft} onRun={armRun} minHeight="140px" />
      </div>
      <div className="form-actions sql-actions">
        <button className="btn primary" disabled={!draft.trim()} onClick={() => armRun()}>
          Run
        </button>
        <button
          className="btn"
          disabled={!draft.trim() || draftIsScript}
          title={draftIsScript ? "Explain works on a single statement" : "Show the query plan (reads only)"}
          onClick={explain}
        >
          Explain
        </button>
        {draftIsScript && (
          <span className="badge script-count">{draftStatements.length} statements</span>
        )}
        <span className="muted run-hint">⌘↩ to run (selection runs alone)</span>
      </div>

      {planErr && <div className="error">{planErr}</div>}
      {plan && (
        <details open className="card explain-plan">
          <summary>
            Query plan
            <button className="btn small plan-close" onClick={() => setPlan(null)} title="Close">
              ×
            </button>
          </summary>
          {plan.plan ? (
            <pre>{plan.plan}</pre>
          ) : (
            <div className="muted">No plan available ({plan.mode}).</div>
          )}
        </details>
      )}

      {/* Single statement — unchanged ApprovalCard flow. */}
      {!isScript && prepared && (
        <ApprovalCard
          key={`${runSeq}:${prepared}`}
          connectionId={connection.id}
          engine={connection.engine}
          sql={prepared}
          safety={safety}
          onExecuted={(o) => {
            setRun({ sql: prepared, outcome: o, at: new Date().toLocaleTimeString() });
            setLimit(STEP);
          }}
          onReject={() => setPrepared(null)}
        />
      )}
      {!isScript && run && (
        <Outcome
          run={run}
          limit={limit}
          maxRows={safety.maxRows}
          onMore={() => setLimit((l) => l + STEP)}
        />
      )}

      {/* Script — confirm panel first, then stacked per-statement results. */}
      {isScript && armed && !scriptOut && (
        <ScriptApproval
          key={`${runSeq}:${armed}`}
          connection={connection}
          safety={safety}
          statements={targetStatements}
          sql={armed}
          onExecuted={(o) => setScriptOut({ outcome: o, at: new Date().toLocaleTimeString() })}
          onCancel={() => {
            // Dismissing the gate returns the console to a neutral state — leaving
            // `target` as a script would keep any earlier single-statement result
            // hidden behind the isScript branch forever.
            setArmed(null);
            setTarget(null);
            setPrepared(null);
            setRun(null);
          }}
        />
      )}
      {isScript && scriptOut && <ScriptResults outcome={scriptOut.outcome} at={scriptOut.at} />}
    </div>
  );
}

// Up-front script gate. Reuses ApprovalCard's visual language (card + badges + actions),
// lists every statement, and hard-gates Execute behind one review checkbox. Nothing here
// is trusted for safety — run_script re-enforces allow_writes + approval (L2).
function ScriptApproval({
  connection,
  safety,
  statements,
  sql,
  onExecuted,
  onCancel,
}: {
  connection: ConnectionProfile;
  safety: SafetySettings;
  statements: string[];
  sql: string;
  onExecuted: (o: ScriptOutcome) => void;
  onCancel: () => void;
}) {
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const queryId = useRef<string | null>(null);
  const cancelledRef = useRef(false);

  async function execute() {
    const id = crypto.randomUUID();
    queryId.current = id;
    cancelledRef.current = false;
    setBusy(true);
    setError(null);
    setCancelled(false);
    try {
      onExecuted(await runScript(connection.id, sql, true, id));
    } catch (e) {
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

  return (
    <div className="card approval script-approval">
      <div className="approval-head">
        <span className="badge kind">SCRIPT</span>
        <span className="badge">{statements.length} statements</span>
        <span className="badge dialect">{ENGINE_LABEL[connection.engine]}</span>
      </div>
      <p className="muted script-note">
        A script that modifies data runs as ONE transaction — all statements commit
        together or none do. A read-only script runs sequentially.
      </p>
      <ol className="stmt-list">
        {statements.map((s, i) => (
          <li key={i}>
            <pre className="stmt">{s}</pre>
          </li>
        ))}
      </ol>
      {!safety.allowWrites && (
        <div className="note muted">
          Writes are disabled for this connection — a script that modifies data will be
          blocked. Enable writes in Safety.
        </div>
      )}
      {error && <div className="error">{error}</div>}
      {cancelled && <div className="muted">Query cancelled.</div>}
      <div className="approval-actions">
        <label className="script-confirm">
          <input
            type="checkbox"
            checked={confirmed}
            onChange={(e) => setConfirmed(e.target.checked)}
          />
          I have reviewed these {statements.length} statements
        </label>
        <button className="btn primary" disabled={busy || !confirmed} onClick={execute}>
          Execute script
        </button>
        {busy ? (
          <button className="btn" onClick={cancel}>
            Cancel query
          </button>
        ) : (
          <button className="btn" onClick={onCancel}>
            Cancel
          </button>
        )}
      </div>
    </div>
  );
}

function ScriptResults({ outcome, at }: { outcome: ScriptOutcome; at: string }) {
  const summary = outcome.allReads
    ? "read-only script"
    : outcome.committed
      ? "script committed (one transaction)"
      : "script failed — rolled back";
  return (
    <div className="results script-results">
      <div className="result-meta muted">
        {summary} · {outcome.statements.length} statements · {at}
      </div>
      {outcome.statements.map((s, i) => (
        <div key={i} className="stmt-result">
          <div className="result-meta muted">
            <span className="stmt-num">{i + 1}</span>
            <code className="result-sql">{s.sql}</code>
          </div>
          {s.error ? (
            <div className="error">{s.error}</div>
          ) : s.result ? (
            <>
              <div className="muted stmt-rowmeta">
                {s.result.rowCount} rows{s.result.truncated && " (truncated)"} ·{" "}
                {s.result.durationMs} ms
                {" · "}
                <ResultToolbar
                  columns={s.result.columns}
                  rows={s.result.rows}
                  filenameBase={`script-stmt${i + 1}-${stamp()}`}
                />
              </div>
              <DataGrid result={s.result} />
            </>
          ) : (
            <div className="muted">{s.affected ?? 0} affected</div>
          )}
        </div>
      ))}
    </div>
  );
}

function Outcome({
  run,
  limit,
  maxRows,
  onMore,
}: {
  run: Run;
  limit: number;
  maxRows: number;
  onMore: () => void;
}) {
  const { outcome, sql, at } = run;
  const r = outcome.result;

  return (
    <div className="results">
      <div className="result-meta muted">
        <code className="result-sql">{sql}</code>
        {r ? (
          <>
            {" · "}
            {r.rowCount} rows
            {r.truncated && ` — capped at ${maxRows} rows — add LIMIT to see more`} ·{" "}
            {r.durationMs} ms · {at}
            {" · "}
            <ResultToolbar
              columns={r.columns}
              rows={r.rows}
              filenameBase={`query-${stamp()}`}
            />
          </>
        ) : (
          <>
            {" · "}
            {outcome.committed ? "write committed" : "no rows returned"}
            {outcome.affected !== null && <> · {outcome.affected} affected</>} · {at}
          </>
        )}
      </div>
      {r && (
        <>
          <DataGrid result={limit < r.rows.length ? { ...r, rows: r.rows.slice(0, limit) } : r} />
          {r.rows.length > limit && (
            <button className="btn" onClick={onMore}>
              Show {Math.min(STEP, r.rows.length - limit)} more of {r.rows.length}
            </button>
          )}
        </>
      )}
    </div>
  );
}
