// Manual SQL console. Editable CodeMirror. A single statement goes through the exact
// same ApprovalCard safety gate as agent output. A pasted multi-statement script (>1)
// switches to script mode: an up-front confirm panel (list + one checkbox + Execute),
// then per-statement results stacked below. ⌘↩ runs the current draft. The draft lives
// in App so it survives switching tabs.
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  Catalog,
  ConnectionProfile,
  Engine,
  ExecOutcome,
  SafetySettings,
  ScriptOutcome,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { cancelQuery, classifySql, getCatalog, previewSql, runScript } from "../../ipc/commands";
import type { PreviewReport } from "../../ipc/types";
import { splitStatements } from "../../lib/sqlStatements";
import ApprovalCard from "../../components/ApprovalCard";
import { Icon } from "../../components/Icon";
import LazySqlViewer from "../../components/LazySqlViewer";
import DataGrid from "../../components/DataGrid";
import ResultToolbar from "../../components/ResultToolbar";
import { stamp } from "../../lib/export";
import { useI18n } from "../../lib/i18n";
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
  const { t } = useI18n();
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
  const [explaining, setExplaining] = useState(false);

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
    if (!draft.trim() || draftIsScript || explaining) return;
    setPlanErr(null);
    setExplaining(true);
    try {
      // Reads only: preview_sql on a write does an execute+rollback (locks, triggers) —
      // that impact preview belongs to the Run approval card, not a casual Explain.
      const cls = await classifySql(connection.id, draft);
      if (cls.kind !== "read") {
        setPlan(null);
        setPlanErr(t("sql.explainReadOnly"));
        return;
      }
      setPlan(await previewSql(connection.id, draft));
    } catch (e) {
      setPlanErr(errMessage(e));
      setPlan(null);
    } finally {
      setExplaining(false);
    }
  }

  // A plan describes the draft it was generated from — invalidate it on edit.
  useEffect(() => {
    setPlan(null);
    setPlanErr(null);
  }, [draft]);

  // #8: feed schema-aware autocomplete. Same introspected Catalog the sidebar tree uses
  // (columns per table). Cache by connection id; failure just leaves completion off.
  const catalogCache = useRef<Record<string, Catalog>>({});
  const [catalog, setCatalog] = useState<Catalog | undefined>(undefined);
  useEffect(() => {
    const id = connection.id;
    const cached = catalogCache.current[id];
    if (cached) {
      setCatalog(cached);
      return;
    }
    setCatalog(undefined);
    let alive = true;
    getCatalog(id)
      .then((c) => {
        catalogCache.current[id] = c;
        if (alive) setCatalog(c);
      })
      .catch(() => {}); // no catalog → editor still works, just no schema hints
    return () => {
      alive = false;
    };
  }, [connection.id]);

  return (
    <div className="screen sqlconsole">
      <div className="editor-box">
        <LazySqlViewer
          value={draft}
          editable
          onChange={setDraft}
          onRun={armRun}
          catalog={catalog}
          minHeight="140px"
        />
      </div>
      <div className="form-actions sql-actions">
        <button className="btn primary" disabled={!draft.trim()} onClick={() => armRun()}>
          {t("sql.run")}
        </button>
        <button
          className="btn"
          disabled={!draft.trim() || draftIsScript || explaining}
          title={draftIsScript ? t("sql.explainSingle") : t("sql.explainTitle")}
          onClick={explain}
        >
          {explaining ? t("sql.planning") : t("sql.explain")}
        </button>
        {draftIsScript && (
          <span className="badge script-count">
            {t("sql.statementCount", { count: draftStatements.length })}
          </span>
        )}
        <span className="muted run-hint">{t("sql.runHint")}</span>
      </div>

      {planErr && <div className="error">{planErr}</div>}
      {plan && (
        <details open className="card explain-plan">
          <summary>
            {t("sql.queryPlan")}
            <button className="btn small plan-close" onClick={() => setPlan(null)} title={t("common.close")} aria-label={t("common.close")}>
              <Icon name="close" />
            </button>
          </summary>
          {plan.plan ? (
            <pre>{plan.plan}</pre>
          ) : (
            <div className="muted">{t("sql.noPlan", { mode: plan.mode })}</div>
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
  const { t } = useI18n();
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
        <span className="badge">{t("sql.statementCount", { count: statements.length })}</span>
        <span className="badge dialect">{ENGINE_LABEL[connection.engine]}</span>
      </div>
      <p className="muted script-note">{t("sql.scriptNote")}</p>
      <ol className="stmt-list">
        {statements.map((s, i) => (
          <li key={i}>
            <pre className="stmt">{s}</pre>
          </li>
        ))}
      </ol>
      {!safety.allowWrites && (
        <div className="note muted">{t("sql.writesDisabledScript")}</div>
      )}
      {error && <div className="error">{error}</div>}
      {cancelled && <div className="muted">{t("sql.cancelled")}</div>}
      <div className="approval-actions">
        <label className="script-confirm">
          <input
            type="checkbox"
            checked={confirmed}
            onChange={(e) => setConfirmed(e.target.checked)}
          />
          {t("sql.confirmReviewed", { count: statements.length })}
        </label>
        <button className="btn primary" disabled={busy || !confirmed} onClick={execute}>
          {t("sql.executeScript")}
        </button>
        {busy ? (
          <button className="btn" onClick={cancel}>
            {t("sql.cancel")}
          </button>
        ) : (
          <button className="btn" onClick={onCancel}>
            {t("common.cancel")}
          </button>
        )}
      </div>
    </div>
  );
}

function ScriptResults({ outcome, at }: { outcome: ScriptOutcome; at: string }) {
  const { t } = useI18n();
  const summary = outcome.allReads
    ? t("sql.readOnlyScript")
    : outcome.committed
      ? t("sql.committed")
      : t("sql.failedRolledBack");
  return (
    <div className="results script-results">
      <div className="result-meta muted">
        {summary} · {t("sql.statementCount", { count: outcome.statements.length })} · {at}
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
                {t(s.result.truncated ? "agent.rowsTruncated" : "agent.rows", {
                  count: s.result.rowCount,
                })} ·{" "}
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
            <div className="muted">{t("sql.affected", { count: s.affected ?? 0 })}</div>
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
  const { t } = useI18n();
  const { outcome, sql, at } = run;
  const r = outcome.result;

  return (
    <div className="results">
      <div className="result-meta muted">
        <code className="result-sql">{sql}</code>
        {r ? (
          <>
            {" · "}
            {t(r.truncated ? "agent.rowsTruncated" : "agent.rows", {
              count: r.rowCount,
            })}
            {r.truncated && ` - ${t("sql.capped", { count: maxRows })}`} ·{" "}
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
            {outcome.committed ? t("sql.writeCommitted") : t("sql.noRowsReturned")}
            {outcome.affected !== null && <> · {t("sql.affected", { count: outcome.affected })}</>} · {at}
          </>
        )}
      </div>
      {r && (
        <>
          <DataGrid result={limit < r.rows.length ? { ...r, rows: r.rows.slice(0, limit) } : r} />
          {r.rows.length > limit && (
            <button className="btn" onClick={onMore}>
              {t("sql.showMore", {
                count: Math.min(STEP, r.rows.length - limit),
                total: r.rows.length,
              })}
            </button>
          )}
        </>
      )}
    </div>
  );
}
