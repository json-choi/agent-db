// The live agent result grid + activity feed, rendered in the app-level Agent tab.
// Auto-shows the newest result as it streams in; clicking a feed row overrides the shown
// result until the next arrives.
import { useEffect, useState, type KeyboardEvent } from "react";
import DataGrid from "./DataGrid";
import ResultToolbar from "./ResultToolbar";
import { stamp } from "../lib/export";
import { fullTime } from "../lib/relTime";
import { useAgentFeed, type AgentActivity } from "../lib/agentFeed";

export default function AgentResultView({ onOpenMcpSettings }: { onOpenMcpSettings?: () => void }) {
  const { feed, latest } = useAgentFeed();
  const [selected, setSelected] = useState<AgentActivity | null>(latest);

  // Only auto-follow the stream while the user is already on the newest result; if they
  // clicked back to an older one, don't yank it out from under them mid-read.
  const following = !selected || selected.id === latest?.id;
  useEffect(() => {
    if (latest && following) setSelected(latest);
  }, [latest, following]);

  return (
    <>
      {/* Live result the agent just queried — visible right here in the app. */}
      <h3>Agent result</h3>
      {!following && latest && (
        <button className="btn small" onClick={() => setSelected(latest)}>
          Jump to latest
        </button>
      )}
      {selected?.rowsDropped ? (
        <div className="muted">result no longer cached — re-run to view</div>
      ) : selected?.result ? (
        <div className="mcp-result">
          <div className="mcp-result-head">
            {selected.sql ? <code className="mcp-result-sql">{selected.sql}</code> : <span className="muted">{selected.tool}</span>}
            <span className="muted">
              {selected.connection ? `${selected.connection} · ` : ""}
              {selected.result.rowCount} rows{selected.result.truncated ? " (truncated)" : ""} ·{" "}
              <span title={fullTime(selected.iso)}>{selected.ts}</span>
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
              <span className="act-ts" title={fullTime(a.iso)}>{a.ts}</span>
              <span className="act-tool">{a.tool}</span>
              <span className="act-kind">{a.kind === "call" ? "→" : "✓"}</span>
              <span className="act-detail" title={a.detail}>{a.detail}</span>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
