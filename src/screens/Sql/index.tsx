// Manual SQL console. Editable CodeMirror. Run is the human approval action for
// manual SQL, so execution stays in-place: action bar status first, results below.
// Multi-statement scripts execute through the backend script runner and return
// per-statement results. ⌘↩ runs the current draft or selected SQL.
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  Catalog,
  ConnectionProfile,
  ExecOutcome,
  PlatformInfo,
  SafetySettings,
  ScriptOutcome,
} from "../../ipc/types";
import { errDetails, errMessage, type AppErrorDetails } from "../../ipc/types";
import {
  cancelQuery,
  classifySql,
  getCatalog,
  mcpPlatforms,
  openAgentApp,
  previewSql,
  runScript,
  runSql,
} from "../../ipc/commands";
import type { PreviewReport } from "../../ipc/types";
import { splitStatements } from "../../lib/sqlStatements";
import { Icon } from "../../components/Icon";
import InfoTip from "../../components/InfoTip";
import LazySqlViewer from "../../components/LazySqlViewer";
import DataGrid from "../../components/DataGrid";
import ResultToolbar from "../../components/ResultToolbar";
import { stamp } from "../../lib/export";
import { useI18n } from "../../lib/i18n";
import { useToast } from "../../components/Toast";
import "./sql.css";

const STEP = 200;

interface Run {
  sql: string;
  outcome: ExecOutcome;
  at: string;
}

interface QueryErrorInfo extends AppErrorDetails {
  sql: string;
  at: string;
}

interface LastAttempt {
  sql: string;
  at: string;
}

type ResultKind = "single" | "script";

interface RunSignal {
  tone: "muted" | "warning" | "danger";
  text: string;
  title?: string;
  icon?: "alert" | "info";
}

type Translate = ReturnType<typeof useI18n>["t"];

function buildSqlHelpPrompt({
  connection,
  sql,
  error,
}: {
  connection: ConnectionProfile;
  sql: string;
  error: QueryErrorInfo | null;
}) {
  const lines = [
    "DopeDB SQL context",
    "",
    `Connection: ${connection.name || "(unnamed)"}`,
    `Engine: ${connection.engine}`,
    `Database: ${connection.database}`,
    "",
    "SQL:",
    "```sql",
    sql.trim(),
    "```",
  ];
  if (error) {
    lines.push(
      "",
      "Error:",
      error.kind ? `Kind: ${error.kind}` : "Kind: unknown",
      `Message: ${error.message}`,
      "",
      "Raw error:",
      "```json",
      error.raw,
      "```",
    );
  }
  return lines.join("\n");
}

function compactSql(sql: string): string {
  return sql
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/--[^\n]*/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function likelyMutates(sql: string): boolean {
  return /^(insert|update|delete|merge|replace|create|alter|drop|truncate|grant|revoke|vacuum|analyze|call|execute)\b/i.test(
    compactSql(sql),
  );
}

function likelyRead(sql: string): boolean {
  return /^(select|with|show|describe|desc|explain)\b/i.test(compactSql(sql));
}

function lacksWhereOnBulkMutation(sql: string): boolean {
  const compact = compactSql(sql);
  return /^(update|delete)\b/i.test(compact) && !/\bwhere\b/i.test(compact);
}

function likelyHeavyRead(sql: string): boolean {
  const compact = compactSql(sql);
  return likelyRead(compact) && /\b(cross\s+join|generate_series)\b/i.test(compact);
}

function likelyUnboundedRead(sql: string): boolean {
  const compact = compactSql(sql);
  return likelyRead(compact) && !/\blimit\s+\d+\b/i.test(compact);
}

function buildRunSignal(
  sql: string,
  statements: string[],
  safety: SafetySettings,
  t: Translate,
): RunSignal | null {
  if (!sql.trim()) return null;
  const effectiveStatements = statements.length > 0 ? statements : [sql];
  const writes = effectiveStatements.some(likelyMutates);

  if (effectiveStatements.length > 1) {
    if (writes && !safety.allowWrites) {
      return {
        tone: "danger",
        icon: "alert",
        text: t("sql.signalWritesDisabled"),
        title: t("sql.writesDisabledScript"),
      };
    }
    if (effectiveStatements.length >= 12) {
      return {
        tone: "warning",
        icon: "alert",
        text: t("sql.signalLargeScript", { count: effectiveStatements.length }),
        title: t("sql.scriptNote"),
      };
    }
    if (writes) {
      return {
        tone: "warning",
        icon: "alert",
        text: t("sql.signalWriteScript"),
        title: t("sql.scriptNote"),
      };
    }
    return {
      tone: "muted",
      icon: "info",
      text: t("sql.signalReadScript", { count: effectiveStatements.length }),
    };
  }

  const statement = effectiveStatements[0] ?? sql;
  if (lacksWhereOnBulkMutation(statement)) {
    return {
      tone: "warning",
      icon: "alert",
      text: t("sql.signalNoWhere"),
    };
  }
  if (/^explain\s+analyze\b/i.test(compactSql(statement))) {
    return {
      tone: "warning",
      icon: "alert",
      text: t("sql.signalExplainAnalyze"),
    };
  }
  if (likelyMutates(statement)) {
    if (!safety.allowWrites) {
      return {
        tone: "danger",
        icon: "alert",
        text: t("sql.signalWritesDisabled"),
      };
    }
    return {
      tone: "warning",
      icon: "alert",
      text: t("sql.signalWriteStatement"),
    };
  }
  if (likelyHeavyRead(statement)) {
    return {
      tone: "warning",
      icon: "alert",
      text: t("sql.signalHeavyRead"),
    };
  }
  if (likelyUnboundedRead(statement)) {
    return {
      tone: "muted",
      icon: "info",
      text: t("sql.signalReadCap", { count: safety.maxRows }),
    };
  }
  return null;
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
  const toast = useToast();
  const draftStatements = useMemo(() => splitStatements(draft), [draft]);
  const draftIsScript = draftStatements.length > 1;
  const draftSignal = useMemo(
    () => buildRunSignal(draft, draftStatements, safety, t),
    [draft, draftStatements, safety, t],
  );

  const [resultKind, setResultKind] = useState<ResultKind | null>(null);
  const [run, setRun] = useState<Run | null>(null);
  const [limit, setLimit] = useState(STEP);
  const [scriptOut, setScriptOut] = useState<{ outcome: ScriptOutcome; at: string } | null>(null);
  const [running, setRunning] = useState(false);
  const [runErr, setRunErr] = useState<QueryErrorInfo | null>(null);
  const [lastAttempt, setLastAttempt] = useState<LastAttempt | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const queryId = useRef<string | null>(null);
  const cancelledRef = useRef(false);
  const [agentPlatforms, setAgentPlatforms] = useState<PlatformInfo[]>([]);
  const [agentErr, setAgentErr] = useState<string | null>(null);
  const [askingAgent, setAskingAgent] = useState<string | null>(null);

  // EXPLAIN plan (read-only preview) shown above the results, independent of execution.
  const [plan, setPlan] = useState<PreviewReport | null>(null);
  const [planErr, setPlanErr] = useState<string | null>(null);
  const [explaining, setExplaining] = useState(false);

  async function executeSql(selectedSql?: string) {
    const sql = selectedSql?.trim() || draft.trim();
    if (!sql || running) return;

    const statements = splitStatements(sql);
    const script = statements.length > 1;
    const id = crypto.randomUUID();
    const at = new Date().toLocaleTimeString();
    queryId.current = id;
    cancelledRef.current = false;
    setRunning(true);
    setRunErr(null);
    setCancelled(false);
    setLimit(STEP);
    setResultKind(script ? "script" : "single");
    setLastAttempt({ sql, at });
    if (script) setScriptOut(null);
    else setRun(null);

    try {
      if (script) {
        const outcome = await runScript(connection.id, sql, true, id);
        setScriptOut({ outcome, at });
      } else {
        const outcome = await runSql(connection.id, sql, true, id);
        setRun({ sql, outcome, at });
      }
    } catch (e) {
      if (cancelledRef.current) setCancelled(true);
      else {
        const details = errDetails(e);
        setRunErr({ ...details, sql, at: new Date().toLocaleTimeString() });
      }
    } finally {
      queryId.current = null;
      setRunning(false);
    }
  }

  function cancelRun() {
    if (queryId.current) {
      cancelledRef.current = true;
      void cancelQuery(queryId.current);
    }
  }

  async function explain() {
    if (!draft.trim() || draftIsScript || explaining) return;
    setPlanErr(null);
    setExplaining(true);
    try {
      // Reads only: preview_sql on a write does an execute+rollback (locks, triggers) —
      // keep that impact out of a casual Explain action.
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

  useEffect(() => {
    if (!running) {
      setElapsed(0);
      return;
    }
    const timer = setInterval(() => setElapsed((s) => s + 1), 1000);
    return () => clearInterval(timer);
  }, [running]);

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

  useEffect(() => {
    let alive = true;
    mcpPlatforms()
      .then((ps) => {
        if (alive) {
          setAgentPlatforms(ps);
          setAgentErr(null);
        }
      })
      .catch((e) => {
        if (alive) setAgentErr(errMessage(e));
      });
    return () => {
      alive = false;
    };
  }, []);

  const openTargets = useMemo(
    () =>
      agentPlatforms.filter(
        (p) => p.installed && (p.id === "codex-desktop" || p.id === "claude-desktop"),
      ),
    [agentPlatforms],
  );
  const promptSql = lastAttempt?.sql || draft;
  const aiPrompt = useMemo(
    () => buildSqlHelpPrompt({ connection, sql: promptSql, error: runErr }),
    [connection, promptSql, runErr],
  );

  function agentLabel(platform: PlatformInfo) {
    return platform.id.startsWith("codex") ? "Codex" : "Claude";
  }

  async function openAgent(platform: PlatformInfo) {
    if (askingAgent) return;
    setAskingAgent(platform.id);
    setAgentErr(null);
    try {
      await openAgentApp(platform.id);
      toast(t("sql.openAgentReady", { name: agentLabel(platform) }));
    } catch (e) {
      const msg = errMessage(e);
      setAgentErr(msg);
      toast(msg, "error");
    } finally {
      setAskingAgent(null);
    }
  }

  return (
    <div className="screen sqlconsole">
      {openTargets.length > 0 && (
        <div className="sql-agent-launchers" aria-label={t("sql.openAgentGroup")}>
          {openTargets.map((platform) => {
            const label = agentLabel(platform);
            return (
              <button
                key={platform.id}
                className="btn small sql-agent-btn"
                disabled={!!askingAgent}
                onClick={() => void openAgent(platform)}
                title={t("sql.openAgentTitle", { name: label })}
              >
                {askingAgent === platform.id ? t("mcp.working") : label}
              </button>
            );
          })}
        </div>
      )}
      <div className="editor-box">
        <LazySqlViewer
          value={draft}
          editable
          onChange={setDraft}
          onRun={executeSql}
          catalog={catalog}
          minHeight="clamp(96px, 18vh, 140px)"
        />
      </div>
      <div className="form-actions sql-actions">
        <button
          className="btn primary"
          disabled={!draft.trim() || running}
          onClick={() => void executeSql()}
        >
          <Icon name="play" />
          {running ? t("sql.running") : t("sql.run")}
        </button>
        <button
          className="btn"
          disabled={!draft.trim() || draftIsScript || explaining || running}
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
        {running ? (
          <>
            <span
              className="badge icon-only-badge"
              title={t("sql.runningFor", { seconds: elapsed })}
              aria-label={t("sql.runningFor", { seconds: elapsed })}
              role="img"
            >
              <Icon name="refresh" />
            </span>
            <button className="btn" onClick={cancelRun}>
              {t("sql.cancel")}
            </button>
          </>
        ) : (
          draftSignal && (
            <span
              className={
                "badge icon-only-badge" +
                (draftSignal.tone === "danger"
                  ? " status-error"
                  : draftSignal.tone === "warning"
                    ? " risk-medium"
                    : "")
              }
              title={draftSignal.title ?? draftSignal.text}
              aria-label={draftSignal.text}
              role="img"
            >
              <Icon name={draftSignal.icon ?? "info"} />
            </span>
          )
        )}
        <InfoTip label={t("sql.runHint")} className="run-hint" />
      </div>

      {agentErr && <div className="error sql-agent-error">{agentErr}</div>}

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

      {runErr && (
        <SqlErrorCard error={runErr} prompt={aiPrompt} />
      )}
      {cancelled && <div className="muted sql-run-message">{t("sql.cancelled")}</div>}

      {resultKind === "single" && run && (
        <Outcome
          run={run}
          limit={limit}
          maxRows={safety.maxRows}
          onMore={() => setLimit((l) => l + STEP)}
        />
      )}
      {resultKind === "script" && scriptOut && (
        <ScriptResults outcome={scriptOut.outcome} at={scriptOut.at} />
      )}
    </div>
  );
}

function SqlErrorCard({
  error,
  prompt,
}: {
  error: QueryErrorInfo;
  prompt: string;
}) {
  const { t } = useI18n();
  return (
    <div className="sql-error-card" role="alert">
      <div className="sql-error-head">
        <div>
          <strong>{t("sql.errorTitle")}</strong>
          <span className="muted"> · {error.at}</span>
        </div>
      </div>
      <div className="sql-error-grid">
        <span className="muted">{t("sql.errorKind")}</span>
        <code>{error.kind ?? t("common.unknown")}</code>
        <span className="muted">{t("sql.errorMessage")}</span>
        <pre>{error.message}</pre>
      </div>
      <details className="sql-error-context">
        <summary>{t("sql.errorContext")}</summary>
        <pre>{prompt}</pre>
      </details>
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
