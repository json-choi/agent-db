// Agent workspace: result review, MCP context ledger, and policy/audit posture.
import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import DataGrid from "./DataGrid";
import DashboardDraftPanel from "./DashboardDraftPanel";
import ResultToolbar from "./ResultToolbar";
import { Icon } from "./Icon";
import InfoTip from "./InfoTip";
import { stamp } from "../lib/export";
import { fullTime } from "../lib/relTime";
import { useAgentFeed, type AgentActivity } from "../lib/agentFeed";
import { useI18n, type I18nKey } from "../lib/i18n";
import type { Dashboard } from "../ipc/types";
import "./AgentResultView.css";

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

function AgentEmptyState() {
  const { t } = useI18n();
  const items: {
    icon: "database" | "play" | "circleSlash" | "alert";
    title: I18nKey;
    body: I18nKey;
    tone: "trust" | "risk" | "danger";
  }[] = [
    {
      icon: "database",
      title: "agent.schemaAccess",
      body: "agent.schemaAccessBody",
      tone: "trust",
    },
    {
      icon: "play",
      title: "agent.dataAccess",
      body: "agent.dataAccessBody",
      tone: "trust",
    },
    {
      icon: "circleSlash",
      title: "agent.schemaModification",
      body: "agent.schemaModificationBody",
      tone: "risk",
    },
    {
      icon: "alert",
      title: "agent.dataModification",
      body: "agent.dataModificationBody",
      tone: "danger",
    },
  ];

  return (
    <div className="agent-empty-panel ds-surface ds-tone-trust">
      <div className="agent-empty-copy">
        <div className="agent-empty-title">
          <h3>{t("agent.ledgerTitle")}</h3>
          <InfoTip label={t("agent.emptyBody")} />
        </div>
      </div>
      <div className="agent-empty-rows ds-card-grid" aria-label={t("agent.emptyCards")}>
        {items.map((item) => (
          <div
            className={`agent-empty-row ds-card ds-card-stack ds-tone-${item.tone}`}
            key={item.title}
            title={t(item.body)}
          >
            <div className="ds-card-title-row">
              <Icon name={item.icon} />
              <strong>{t(item.title)}</strong>
              <InfoTip label={t(item.body)} />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export default function AgentResultView({
  onDashboardSaved,
}: {
  onDashboardSaved: (dashboard: Dashboard) => void;
}) {
  const { t } = useI18n();
  const { feed, latest } = useAgentFeed();
  const [view, setView] = useState<AgentView>("result");
  const [selected, setSelected] = useState<AgentActivity | null>(() =>
    selectedResult(feed, latest),
  );
  const [dashboardSourceId, setDashboardSourceId] = useState<number | null>(null);

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
          <div className="agent-title-row">
            <h2>{t("agent.workspace")}</h2>
            <InfoTip label={t("agent.contextHelp")} />
          </div>
        </div>
        <div className="agent-stats" aria-label={t("agent.session")}>
          <span title={t("agent.toolCalls", { count: stats.calls })} aria-label={t("agent.toolCalls", { count: stats.calls })}>
            <Icon name="play" />
            {stats.calls}
          </span>
          <span title={t("agent.results", { count: stats.results })} aria-label={t("agent.results", { count: stats.results })}>
            <Icon name="database" />
            {stats.results}
          </span>
          <span title={t("agent.errorCount", { count: stats.errors })} aria-label={t("agent.errorCount", { count: stats.errors })}>
            <Icon name={stats.errors ? "alert" : "check"} />
            {stats.errors}
          </span>
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
        <AgentEmptyState />
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
                  <div className="mcp-result-actions">
                    <ResultToolbar
                      columns={activeResult.result.columns}
                      rows={activeResult.result.rows}
                      filenameBase={`agent-${stamp()}`}
                    />
                    {activeResult.tool === "run_query" &&
                      activeResult.sql &&
                      activeResult.connectionId && (
                        <button
                          className={
                            dashboardSourceId === activeResult.id
                              ? "btn small primary"
                              : "btn small"
                          }
                          onClick={() =>
                            setDashboardSourceId((current) =>
                              current === activeResult.id ? null : activeResult.id,
                            )
                          }
                        >
                          <Icon name="dashboard" />
                          {t("dashboard.saveResult")}
                        </button>
                      )}
                  </div>
                </div>
                {dashboardSourceId === activeResult.id &&
                  activeResult.sql &&
                  activeResult.connectionId && (
                    <DashboardDraftPanel
                      key={activeResult.id}
                      connectionId={activeResult.connectionId}
                      sql={activeResult.sql}
                      result={activeResult.result}
                      onCancel={() => setDashboardSourceId(null)}
                      onSaved={(dashboard) => {
                        setDashboardSourceId(null);
                        onDashboardSaved(dashboard);
                      }}
                    />
                  )}
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
        <div className="agent-audit-grid ds-card-grid">
          <section className="agent-policy-card ds-card ds-card-row ds-tone-trust" title={t("agent.auditReadOnlyBody")}>
            <Icon name="database" />
            <div>
              <strong>{t("agent.auditReadOnly")}</strong>
              <InfoTip label={t("agent.auditReadOnlyBody")} />
            </div>
          </section>
          <section className="agent-policy-card ds-card ds-card-row ds-tone-danger" title={t("agent.auditBlockedWritesBody")}>
            <Icon name="circleSlash" />
            <div>
              <strong>{t("agent.auditBlockedWrites")}</strong>
              <InfoTip label={t("agent.auditBlockedWritesBody")} />
            </div>
          </section>
          <section className="agent-policy-card ds-card ds-card-row" title={t("agent.auditHashChainBody")}>
            <Icon name="check" />
            <div>
              <strong>{t("agent.auditHashChain")}</strong>
              <InfoTip label={t("agent.auditHashChainBody")} />
            </div>
          </section>
          <section className="agent-policy-card wide ds-panel">
            <div className="agent-section-head">
              <h3>{t("agent.policy")}</h3>
              <span
                className={
                  (errorItems.length ? "badge status status-error" : "badge status status-ok") +
                  " icon-only-badge"
                }
                title={
                  errorItems.length
                    ? t("agent.auditErrors", { count: errorItems.length })
                    : t("agent.auditNoErrors")
                }
                aria-label={
                  errorItems.length
                    ? t("agent.auditErrors", { count: errorItems.length })
                    : t("agent.auditNoErrors")
                }
                role="img"
              >
                <Icon name={errorItems.length ? "alert" : "check"} />
              </span>
            </div>
            {errorItems.length ? (
              <Timeline feed={errorItems} selected={selected} onSelect={setSelected} />
            ) : (
              <InfoTip label={t("agent.auditNoErrors")} />
            )}
          </section>
        </div>
      )}
    </div>
  );
}
