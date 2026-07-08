// Agent workspace: result review, MCP context ledger, and policy/audit posture.
import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import DataGrid from "./DataGrid";
import ResultToolbar from "./ResultToolbar";
import { stamp } from "../lib/export";
import { fullTime } from "../lib/relTime";
import { useAgentFeed, type AgentActivity } from "../lib/agentFeed";
import { useI18n, type I18nKey } from "../lib/i18n";

type AgentView = "result" | "context" | "audit";

function selectedResult(feed: AgentActivity[], latest: AgentActivity | null) {
  return latest ?? feed.find((item) => item.result) ?? null;
}

function contextSummaryKey(item: AgentActivity): I18nKey {
  switch (item.tool) {
    case "list_connections":
      return "agent.contextSummaryListConnections";
    case "list_tables":
      return "agent.contextSummaryListTables";
    case "describe_table":
      return "agent.contextSummaryDescribeTable";
    case "run_query":
      return item.sql
        ? "agent.contextSummaryRunQuery"
        : "agent.contextSummaryRunQueryNoSql";
    default:
      return item.error ? "agent.contextSummaryError" : "agent.contextSummaryDefault";
  }
}

function payloadRows(payload: Record<string, unknown> | undefined) {
  if (!payload) return [];
  return Object.entries(payload).filter(([, value]) => value !== undefined && value !== null);
}

function displayValue(value: unknown) {
  if (Array.isArray(value)) {
    if (value.length === 0) return "[]";
    return value.map((v) => (typeof v === "string" ? v : JSON.stringify(v))).join(", ");
  }
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

function Timeline({
  feed,
  selected,
  onSelect,
}: {
  feed: AgentActivity[];
  selected: AgentActivity | null;
  onSelect: (item: AgentActivity) => void;
}) {
  return (
    <ul className="mcp-feed agent-timeline">
      {feed.map((item) => (
        <li
          key={item.id}
          className={
            (item.error ? "act error" : `act ${item.kind}`) +
            (item.result ? " has-result" : "") +
            (selected?.id === item.id ? " sel" : "")
          }
          role="button"
          tabIndex={0}
          onClick={() => onSelect(item)}
          onKeyDown={(e: KeyboardEvent) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              onSelect(item);
            }
          }}
        >
          <span className="act-ts" title={fullTime(item.iso)}>{item.ts}</span>
          <span className="act-tool">{item.tool}</span>
          <span className="act-kind">{item.kind === "call" ? "->" : "ok"}</span>
          <span className="act-detail" title={item.detail}>{item.detail}</span>
        </li>
      ))}
    </ul>
  );
}

export default function AgentResultView({ onOpenMcpSettings }: { onOpenMcpSettings?: () => void }) {
  const { t } = useI18n();
  const { feed, latest } = useAgentFeed();
  const [view, setView] = useState<AgentView>("result");
  const [selected, setSelected] = useState<AgentActivity | null>(() =>
    selectedResult(feed, latest),
  );

  // Auto-follow only while the user is already on the newest event/result.
  const following = !selected || selected.id === latest?.id;
  useEffect(() => {
    if (latest && following) setSelected(latest);
  }, [latest, following]);
  useEffect(() => {
    if (!selected && feed[0]) setSelected(feed[0]);
  }, [feed, selected]);

  const stats = useMemo(() => {
    const calls = feed.filter((item) => item.kind === "call").length;
    const results = feed.filter((item) => item.kind === "result").length;
    const errors = feed.filter((item) => item.error).length;
    return { calls, results, errors };
  }, [feed]);
  const activeResult = selected?.result ? selected : selectedResult(feed, latest);
  const errorItems = feed.filter((item) => item.error);

  return (
    <div className="agent-workspace">
      <header className="agent-head">
        <div>
          <h2>{t("agent.workspace")}</h2>
          <p className="muted">{t("agent.contextHelp")}</p>
        </div>
        <div className="agent-stats" aria-label={t("agent.session")}>
          <span>{t("agent.toolCalls", { count: stats.calls })}</span>
          <span>{t("agent.results", { count: stats.results })}</span>
          <span>{t("agent.errorCount", { count: stats.errors })}</span>
        </div>
      </header>

      <div className="agent-view-tabs" role="tablist">
        {(["result", "context", "audit"] as AgentView[]).map((id) => (
          <button
            key={id}
            className={view === id ? "seg active" : "seg"}
            role="tab"
            aria-selected={view === id}
            onClick={() => setView(id)}
          >
            {id === "result" ? t("agent.result") : id === "context" ? t("agent.context") : t("agent.audit")}
          </button>
        ))}
      </div>

      {feed.length === 0 ? (
        <div className="agent-empty muted">
          {t("agent.empty")}{" "}
          {onOpenMcpSettings && (
            <button className="btn small" onClick={onOpenMcpSettings}>
              {t("mcp.server")}
            </button>
          )}
        </div>
      ) : view === "result" ? (
        <div className="agent-split">
          <section className="agent-primary">
            <div className="agent-section-head">
              <h3>{t("agent.result")}</h3>
              {!following && latest && (
                <button className="btn small" onClick={() => setSelected(latest)}>
                  {t("agent.jumpLatest")}
                </button>
              )}
            </div>
            {activeResult?.rowsDropped ? (
              <div className="muted">{t("agent.resultDropped")}</div>
            ) : activeResult?.result ? (
              <div className="mcp-result">
                <div className="mcp-result-head">
                  {activeResult.sql ? (
                    <code className="mcp-result-sql">{activeResult.sql}</code>
                  ) : (
                    <span className="muted">{activeResult.tool}</span>
                  )}
                  <span className="muted">
                    {activeResult.connection ? `${activeResult.connection} · ` : ""}
                    {t(
                      activeResult.result.truncated ? "agent.rowsTruncated" : "agent.rows",
                      { count: activeResult.result.rowCount },
                    )}{" "}
                    · <span title={fullTime(activeResult.iso)}>{activeResult.ts}</span>
                  </span>
                  <ResultToolbar
                    columns={activeResult.result.columns}
                    rows={activeResult.result.rows}
                    filenameBase={`agent-${stamp()}`}
                  />
                </div>
                <DataGrid result={activeResult.result} />
              </div>
            ) : (
              <div className="muted">{t("agent.resultHelp")}</div>
            )}
          </section>
          <aside className="agent-secondary">
            <h3>{t("agent.timeline")}</h3>
            <Timeline feed={feed} selected={selected} onSelect={setSelected} />
          </aside>
        </div>
      ) : view === "context" ? (
        <div className="agent-split">
          <section className="agent-primary">
            <div className="agent-section-head">
              <h3>{t("agent.contextExposed")}</h3>
              {selected && <span className="muted">{selected.tool} · {selected.ts}</span>}
            </div>
            {selected ? (
              <div className="context-card">
                <p>{t(contextSummaryKey(selected))}</p>
                {selected.sql && <code className="context-sql">{selected.sql}</code>}
                <div className="context-grid">
                  {payloadRows(selected.payload).map(([key, value]) => (
                    <div className="context-row" key={key}>
                      <span>{key}</span>
                      <code>{displayValue(value)}</code>
                    </div>
                  ))}
                </div>
              </div>
            ) : (
              <p className="muted">{t("agent.noSelection")}</p>
            )}
          </section>
          <aside className="agent-secondary">
            <h3>{t("agent.timeline")}</h3>
            <Timeline feed={feed} selected={selected} onSelect={setSelected} />
          </aside>
        </div>
      ) : (
        <div className="agent-audit-grid">
          <section className="agent-policy-card">
            <h3>{t("agent.auditReadOnly")}</h3>
            <p className="muted">{t("agent.auditReadOnlyBody")}</p>
          </section>
          <section className="agent-policy-card">
            <h3>{t("agent.auditBlockedWrites")}</h3>
            <p className="muted">{t("agent.auditBlockedWritesBody")}</p>
          </section>
          <section className="agent-policy-card">
            <h3>{t("agent.auditHashChain")}</h3>
            <p className="muted">{t("agent.auditHashChainBody")}</p>
          </section>
          <section className="agent-policy-card wide">
            <div className="agent-section-head">
              <h3>{t("agent.policy")}</h3>
              <span className={errorItems.length ? "badge status status-error" : "badge status status-ok"}>
                {errorItems.length
                  ? t("agent.auditErrors", { count: errorItems.length })
                  : t("agent.auditNoErrors")}
              </span>
            </div>
            {errorItems.length ? (
              <Timeline feed={errorItems} selected={selected} onSelect={setSelected} />
            ) : (
              <p className="muted">{t("agent.auditNoErrors")}</p>
            )}
          </section>
        </div>
      )}
    </div>
  );
}
