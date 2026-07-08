// MCP status + one-click connect + LIVE result viewer + activity feed. The feed itself
// lives app-wide in AgentFeedProvider (so agent activity is captured even when this screen
// is closed); here we render it. Runtime status is the REAL listener state (mcp_runtime_status),
// not the static config — a bind failure shows a red banner, not a fake "Running".
import { useEffect, useState } from "react";
import {
  connectPlatform,
  disconnectPlatform,
  mcpPlatforms,
  mcpRuntimeStatus,
  mcpStatus,
  type McpRuntimeStatus,
  type McpStatus,
} from "../../ipc/commands";
import type { PlatformInfo } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import ConfirmButton from "../../components/ConfirmButton";
import { useToast } from "../../components/Toast";

interface Result {
  ok: boolean;
  msg: string;
}

export default function Mcp({ onOpenAgent }: { onOpenAgent?: () => void }) {
  const toast = useToast();
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [runtime, setRuntime] = useState<McpRuntimeStatus | null>(null);
  const [statusErr, setStatusErr] = useState<string | null>(null);
  const [platforms, setPlatforms] = useState<PlatformInfo[]>([]);
  const [platErr, setPlatErr] = useState<string | null>(null);
  const [results, setResults] = useState<Record<string, Result>>({});
  const [connecting, setConnecting] = useState<string | null>(null);

  const refreshPlatforms = () =>
    mcpPlatforms()
      .then((ps) => {
        setPlatforms(ps);
        setPlatErr(null);
      })
      .catch((e) => setPlatErr(errMessage(e)));

  useEffect(() => {
    mcpStatus()
      .then((s) => {
        setStatus(s);
        setStatusErr(null);
      })
      .catch((e) => setStatusErr(errMessage(e)));
    void refreshPlatforms();
    // The server binds in a spawned setup task; if our first read wins the race we'd show a
    // false "not running" banner. Poll until it reports running, then stop.
    let iv: ReturnType<typeof setInterval>;
    const poll = () =>
      mcpRuntimeStatus()
        .then((r) => {
          setRuntime(r);
          if (r.httpRunning) clearInterval(iv);
        })
        .catch((e) => setStatusErr(errMessage(e)));
    void poll();
    iv = setInterval(poll, 2000);
    return () => clearInterval(iv);
  }, []);

  async function runPlatformAction(
    id: string,
    action: (platform: string) => Promise<string>,
    toastLabel: (name: string) => string,
  ) {
    setConnecting(id);
    try {
      const msg = await action(id);
      setResults((r) => ({ ...r, [id]: { ok: true, msg } }));
      toast(toastLabel(platforms.find((p) => p.id === id)?.name ?? id));
      await refreshPlatforms();
    } catch (e) {
      const msg = errMessage(e);
      setResults((r) => ({ ...r, [id]: { ok: false, msg } }));
      toast(msg, "error");
    } finally {
      setConnecting(null);
    }
  }

  const connect = (id: string) =>
    runPlatformAction(id, connectPlatform, (name) => `Connected ${name}`);

  async function disconnect(id: string) {
    await runPlatformAction(id, disconnectPlatform, (name) => `Removed from ${name}`);
  }

  const copy = (text: string, label: string) => {
    void navigator.clipboard.writeText(text);
    toast(`${label} copied`);
  };

  const running = !!runtime?.httpRunning;
  const bridge = !!runtime?.bridgeRunning;
  const httpConfig = status
    ? JSON.stringify({ dopedb: { url: status.url, headers: { Authorization: `Bearer ${status.token}` } } }, null, 2)
    : "";
  const desktopConfig = status
    ? JSON.stringify({ mcpServers: { dopedb: { command: status.bridgePath } } }, null, 2)
    : "";

  return (
    <div className="screen mcp">
      <h2>MCP server</h2>

      {running ? (
        <div className="mcp-status">
          <span className="dot-on" /> HTTP running{status && <> · <code>{status.url}</code></>}
          <span className="muted"> (Claude Code · Cursor · VS Code)</span>
        </div>
      ) : (
        <div className="error">
          MCP HTTP server not running.
          {runtime?.error ? ` ${runtime.error}` : statusErr ? ` ${statusErr}` : ""}
        </div>
      )}
      {/* Bridge listener (7687) is the path Claude Desktop / Codex use. If it's down,
          those clients can't reach the app even though HTTP works. */}
      <div className={bridge ? "mcp-status" : "error"}>
        {bridge ? <span className="dot-on" /> : null} stdio bridge{" "}
        {bridge ? "ready" : "down"}
        <span className="muted"> (Claude Desktop · Codex)</span>
        {!bridge && runtime?.error ? ` — ${runtime.error}` : ""}
      </div>

      <h3>Connect your agent (one click)</h3>
      {platErr ? (
        <div className="error">Could not detect AI platforms: {platErr}</div>
      ) : platforms.length === 0 ? (
        <div className="muted">No supported AI platforms detected on this machine.</div>
      ) : (
        <div className="mcp-platforms">
          {platforms.map((p) => (
            <div className="plat" key={p.id}>
              <div className="plat-row">
                <div className="plat-info">
                  <strong>{p.name}</strong>
                  {p.connected && <span className="plat-tag">✓ Connected</span>}
                  <span className="muted"> · {p.installed ? p.note : "not installed"}</span>
                </div>
                <button
                  className={p.connected ? "btn small" : "btn small primary"}
                  disabled={!p.installed || connecting === p.id}
                  onClick={() => void connect(p.id)}
                >
                  {connecting === p.id ? "Working…" : p.connected ? "Reconnect" : "Connect"}
                </button>
                {p.connected && (
                  <ConfirmButton
                    className="btn small danger"
                    confirmLabel="Remove dopedb?"
                    disabled={connecting === p.id}
                    onConfirm={() => void disconnect(p.id)}
                  >
                    Remove
                  </ConfirmButton>
                )}
              </div>
              {results[p.id] && (
                <div className={results[p.id].ok ? "plat-msg ok" : "plat-msg err"}>{results[p.id].msg}</div>
              )}
            </div>
          ))}
        </div>
      )}

      {status && (
        <details className="manual-setup">
          <summary>Manual setup / other platforms (Cursor, VS Code, …)</summary>
          <div className="mcp-field">
            <label>Bearer token</label>
            <div className="mcp-token">
              <code>{status.token}</code>
              <button className="btn small" onClick={() => copy(status.token, "Token")}>
                Copy
              </button>
            </div>
          </div>
          <div className="mcp-field">
            <div className="mcp-field-head">
              <label>HTTP config (Cursor / VS Code / Windsurf)</label>
              <button className="btn small" onClick={() => copy(httpConfig, "HTTP config")}>
                Copy
              </button>
            </div>
            <pre onClick={() => copy(httpConfig, "HTTP config")}>{httpConfig}</pre>
          </div>
          <div className="mcp-field">
            <div className="mcp-field-head">
              <label>Claude Desktop config (stdio bridge)</label>
              <button className="btn small" onClick={() => copy(desktopConfig, "Desktop config")}>
                Copy
              </button>
            </div>
            <pre onClick={() => copy(desktopConfig, "Desktop config")}>{desktopConfig}</pre>
          </div>
        </details>
      )}

      <p className="muted">
        Live agent activity has moved to the Agent tab.{" "}
        {onOpenAgent && (
          <button className="btn small" onClick={onOpenAgent}>
            Open Agent tab
          </button>
        )}
      </p>
    </div>
  );
}
