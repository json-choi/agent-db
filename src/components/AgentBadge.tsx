// Floating app-level badge: a pulsing dot + count of unseen agent events. Visible in
// every view (the feed lives in AgentFeedProvider, not the Mcp screen). Clicking opens
// the top-level Agent activity view; the count clears once that view is shown.
import "./AgentBadge.css";

export default function AgentBadge({ count, onClick }: { count: number; onClick: () => void }) {
  if (count <= 0) return null;
  return (
    <button className="agent-badge" onClick={onClick} title="Agent activity — open viewer">
      <span className="agent-badge-dot" />
      {count} agent event{count === 1 ? "" : "s"}
    </button>
  );
}
