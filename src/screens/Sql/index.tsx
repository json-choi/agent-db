// Manual SQL console. Editable CodeMirror. A single statement goes through the exact
// same ApprovalCard safety gate as agent output. A pasted multi-statement script (>1)
// switches to script mode: an up-front confirm panel (list + one checkbox + Execute),
// then per-statement results stacked below. ⌘↩ runs the current draft. The draft lives
// in App so it survives switching tabs.
import { useMemo, useState } from "react";
import type {
  ConnectionProfile,
  Engine,
  ExecOutcome,
  SafetySettings,
  ScriptOutcome,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { runScript } from "../../ipc/commands";
import { splitStatements } from "../../lib/sqlStatements";
import ApprovalCard from "../../components/ApprovalCard";
import SqlViewer from "../../components/SqlViewer";
import DataGrid from "../../components/DataGrid";
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
  const statements = useMemo(() => splitStatements(draft), [draft]);
  const isScript = statements.length > 1;

  // Single-statement flow.
  const [prepared, setPrepared] = useState<string | null>(null);
  const [run, setRun] = useState<Run | null>(null);
  const [limit, setLimit] = useState(STEP);

  // Script flow.
  const [armed, setArmed] = useState<string | null>(null);
  const [scriptOut, setScriptOut] = useState<{ outcome: ScriptOutcome; at: string } | null>(null);

  // ⌘↩ and the Run button both just ARM the run — the gate (ApprovalCard for a single
  // statement, the confirm panel for a script) is never bypassed.
  function armRun() {
    if (!draft.trim()) return;
    if (isScript) {
      setArmed(draft);
      setScriptOut(null);
    } else {
      setPrepared(draft);
      setRun(null);
    }
  }

  return (
    <div className="screen sqlconsole">
      <div className="editor-box">
        <SqlViewer value={draft} editable onChange={setDraft} onRun={armRun} minHeight="140px" />
      </div>
      <div className="form-actions sql-actions">
        <button className="btn primary" disabled={!draft.trim()} onClick={armRun}>
          Run
        </button>
        {isScript && <span className="badge script-count">{statements.length} statements</span>}
        <span className="muted run-hint">⌘↩ to run</span>
      </div>

      {/* Single statement — unchanged ApprovalCard flow. */}
      {!isScript && prepared && (
        <ApprovalCard
          key={prepared}
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
      {!isScript && run && <Outcome run={run} limit={limit} onMore={() => setLimit((l) => l + STEP)} />}

      {/* Script — confirm panel first, then stacked per-statement results. */}
      {isScript && armed && !scriptOut && (
        <ScriptApproval
          key={armed}
          connection={connection}
          safety={safety}
          statements={statements}
          sql={armed}
          onExecuted={(o) => setScriptOut({ outcome: o, at: new Date().toLocaleTimeString() })}
          onCancel={() => setArmed(null)}
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

  async function execute() {
    setBusy(true);
    setError(null);
    try {
      onExecuted(await runScript(connection.id, sql, true));
    } catch (e) {
      setError(errMessage(e));
    } finally {
      setBusy(false);
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
        <button className="btn" disabled={busy} onClick={onCancel}>
          Cancel
        </button>
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

function Outcome({ run, limit, onMore }: { run: Run; limit: number; onMore: () => void }) {
  const { outcome, sql, at } = run;
  const r = outcome.result;

  return (
    <div className="results">
      <div className="result-meta muted">
        <code className="result-sql">{sql}</code>
        {r ? (
          <>
            {" · "}
            {r.rowCount} rows{r.truncated && " (truncated)"} · {r.durationMs} ms · {at}
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
