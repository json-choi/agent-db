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
import { useI18n } from "../../lib/i18n";
import "./mcp.css";

interface Result {
  ok: boolean;
  msg: string;
}

export default function Mcp({ onOpenAgent }: { onOpenAgent?: () => void }) {
  const { t } = useI18n();
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
    runPlatformAction(id, connectPlatform, (name) =>
      t("mcp.connectedToast", { name }),
    );

  async function disconnect(id: string) {
    await runPlatformAction(id, disconnectPlatform, (name) =>
      t("mcp.removedToast", { name }),
    );
  }

  const copy = (text: string, label: string) => {
    void navigator.clipboard.writeText(text);
    toast(t("mcp.copied", { label }));
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
      <h2>{t("mcp.server")}</h2>

      {running ? (
        <div className="mcp-status">
          <span className="dot-on" /> {t("mcp.httpRunning")}{status && <> · <code>{status.url}</code></>}
          <span className="muted"> (Claude Code · Cursor · VS Code)</span>
        </div>
      ) : (
        <div className="error">
          {t("mcp.httpNotRunning")}
          {runtime?.error ? ` ${runtime.error}` : statusErr ? ` ${statusErr}` : ""}
        </div>
      )}
      {/* Bridge listener (7687) is the path Claude Desktop / Codex use. If it's down,
          those clients can't reach the app even though HTTP works. */}
      <div className={bridge ? "mcp-status" : "error"}>
        {bridge ? <span className="dot-on" /> : null} {t("mcp.stdioBridge")}{" "}
        {bridge ? t("mcp.bridgeReady") : t("mcp.bridgeDown")}
        <span className="muted"> (Claude Desktop · Codex)</span>
        {!bridge && runtime?.error ? ` — ${runtime.error}` : ""}
      </div>

      <h3>{t("mcp.connectAgent")}</h3>
      {platErr ? (
        <div className="error">{t("mcp.platformDetectFailed", { error: platErr })}</div>
      ) : platforms.length === 0 ? (
        <div className="muted">{t("mcp.noPlatforms")}</div>
      ) : (
        <div className="mcp-platforms">
          {platforms.map((p) => (
            <div className="plat" key={p.id}>
              <div className="plat-row">
                <div className="plat-info">
                  <strong>{p.name}</strong>
                  {p.connected && <span className="plat-tag">✓ {t("mcp.connected")}</span>}
                  <span className="muted"> · {p.installed ? p.note : t("mcp.notInstalled")}</span>
                </div>
                <button
                  className={p.connected ? "btn small" : "btn small primary"}
                  disabled={!p.installed || connecting === p.id}
                  onClick={() => void connect(p.id)}
                >
                  {connecting === p.id
                    ? t("mcp.working")
                    : p.connected
                      ? t("mcp.reconnect")
                      : t("mcp.connect")}
                </button>
                {p.connected && (
                  <ConfirmButton
                    className="btn small danger"
                    confirmLabel={t("mcp.removeConfirm")}
                    disabled={connecting === p.id}
                    onConfirm={() => void disconnect(p.id)}
                  >
                    {t("mcp.remove")}
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
          <summary>{t("mcp.manualSetup")}</summary>
          <div className="mcp-field">
            <label>{t("mcp.token")}</label>
            <div className="mcp-token">
              <code>{status.token}</code>
              <button className="btn small" onClick={() => copy(status.token, "Token")}>
                {t("common.copy")}
              </button>
            </div>
          </div>
          <div className="mcp-field">
            <div className="mcp-field-head">
              <label>{t("mcp.httpConfig")}</label>
              <button className="btn small" onClick={() => copy(httpConfig, "HTTP config")}>
                {t("common.copy")}
              </button>
            </div>
            <pre onClick={() => copy(httpConfig, "HTTP config")}>{httpConfig}</pre>
          </div>
          <div className="mcp-field">
            <div className="mcp-field-head">
              <label>{t("mcp.desktopConfig")}</label>
              <button className="btn small" onClick={() => copy(desktopConfig, "Desktop config")}>
                {t("common.copy")}
              </button>
            </div>
            <pre onClick={() => copy(desktopConfig, "Desktop config")}>{desktopConfig}</pre>
          </div>
        </details>
      )}

      <p className="muted">
        {t("mcp.liveMoved")}{" "}
        {onOpenAgent && (
          <button className="btn small" onClick={onOpenAgent}>
            {t("mcp.openAgent")}
          </button>
        )}
      </p>
    </div>
  );
}
