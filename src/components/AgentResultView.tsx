// The live agent result grid + activity feed, rendered in the app-level Agent tab.
// Auto-shows the newest result as it streams in; clicking a feed row overrides the shown
// result until the next arrives.
import { useEffect, useState, type KeyboardEvent } from "react";
import DataGrid from "./DataGrid";
import ResultToolbar from "./ResultToolbar";
import { stamp } from "../lib/export";
import { useAgentFeed, type AgentActivity } from "../lib/agentFeed";

export default function AgentResultView({ onOpenMcpSettings }: { onOpenMcpSettings?: () => void }) {
  const { feed, latest } = useAgentFeed();
  const [selected, setSelected] = useState<AgentActivity | null>(latest);

  useEffect(() => {
    if (latest) setSelected(latest);
  }, [latest]);

  return (
    <>
      {/* Live result the agent just queried — visible right here in the app. */}
      <h3>Agent result</h3>
      {selected?.result ? (
        <div className="mcp-result">
          <div className="mcp-result-head">
            {selected.sql ? <code className="mcp-result-sql">{selected.sql}</code> : <span className="muted">{selected.tool}</span>}
            <span className="muted">
              {selected.connection ? `${selected.connection} · ` : ""}
              {selected.result.rowCount} rows{selected.result.truncated ? " (truncated)" : ""} · {selected.ts}
            </span>
            <ResultToolbar
              columns={selected.result.columns}
              rows={selected.result.rows}
              filenameBase={`agent-${stamp()}`}
            />
          </div>
          <DataGrid result={selected.result} />
        </div>
      ) : (
        <div className="muted">
          When your agent runs a query over MCP, its result (a table or a single metric)
          appears here live.
        </div>
      )}

      <h3>Activity</h3>
      {feed.length === 0 ? (
        <div className="muted">
          No agent calls yet. Connect an AI agent over MCP to see its queries here live.
          {onOpenMcpSettings && (
            <>
              {" "}
              <button className="btn small" onClick={onOpenMcpSettings}>
                MCP settings
              </button>
            </>
          )}
        </div>
      ) : (
        <ul className="mcp-feed">
          {feed.map((a) => (
            <li
              key={a.id}
              className={
                (a.error ? "act error" : `act ${a.kind}`) +
                (a.result ? " has-result" : "") +
                (selected?.id === a.id ? " sel" : "")
              }
              {...(a.result
                ? {
                    role: "button" as const,
                    tabIndex: 0,
                    onKeyDown: (e: KeyboardEvent) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        setSelected(a);
                      }
                    },
                  }
                : {})}
              onClick={() => a.result && setSelected(a)}
            >
              <span className="act-ts">{a.ts}</span>
              <span className="act-tool">{a.tool}</span>
              <span className="act-kind">{a.kind === "call" ? "→" : "✓"}</span>
              <span className="act-detail">{a.detail}</span>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
