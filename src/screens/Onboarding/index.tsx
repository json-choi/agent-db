// First-run onboarding — shown when no database is connected yet. Instead of a blank
// screen, guide the user to connect a database or wire up the MCP server.
export default function Onboarding({
  onNewConnection,
  onOpenMcp,
}: {
  onNewConnection: () => void;
  onOpenMcp: () => void;
}) {
  return (
    <div className="onboarding">
      <div className="onboarding-inner">
        <h1>Welcome to agent-db</h1>
        <p className="lead muted">
          A safe database client for the AI era. Your agent queries and edits databases
          through agent-db, which keeps everything read-only by default, requires a human
          click for writes, and audits every statement.
        </p>

        <div className="onboarding-steps">
          <div className="step-card">
            <div className="step-num">1</div>
            <h3>Connect a database</h3>
            <p className="muted">
              Add a PostgreSQL, MySQL, or SQLite connection. Credentials are stored in your
              macOS Keychain — never in plain text.
            </p>
            <button className="btn primary" onClick={onNewConnection}>
              Add connection
            </button>
          </div>

          <div className="step-card">
            <div className="step-num">2</div>
            <h3>Connect your AI agent</h3>
            <p className="muted">
              agent-db runs a local MCP server. Point Claude Code, Cursor, or Codex at it —
              the chat stays in your platform, while this app shows and controls the data.
            </p>
            <button className="btn" onClick={onOpenMcp}>
              Set up MCP
            </button>
          </div>
        </div>

        <p className="onboarding-foot muted">
          You can do both, or start with just a database and add the agent later.
        </p>
      </div>
    </div>
  );
}
