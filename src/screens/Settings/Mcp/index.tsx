// MCP status + one-click connect + LIVE result viewer + activity feed. The feed itself
// lives app-wide in AgentFeedProvider (so agent activity is captured even when this screen
// is closed); here we render it. Runtime status is the REAL listener state (mcp_runtime_status),
// not the static config — a bind failure shows a red banner, not a fake "Running".
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { connectPlatform, disconnectPlatform } from "../../../ipc/commands";
import { errMessage } from "../../../ipc/types";
import ConfirmButton from "../../../components/ConfirmButton";
import { Icon } from "../../../components/Icon";
import InfoTip from "../../../components/InfoTip";
import Skeleton from "../../../components/Skeleton";
import { useToast } from "../../../components/Toast";
import { useI18n } from "../../../lib/i18n";
import {
  mcpPlatformsQuery,
  mcpRuntimeStatusQuery,
  mcpStatusQuery,
  qk,
} from "../../../lib/queries";
import "./mcp.css";

interface Result {
  ok: boolean;
  msg: string;
}

export default function Mcp() {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const statusQ = useQuery(mcpStatusQuery());
  const runtimeQ = useQuery(mcpRuntimeStatusQuery());
  const platformsQ = useQuery(mcpPlatformsQuery());
  const [results, setResults] = useState<Record<string, Result>>({});
  const [connecting, setConnecting] = useState<string | null>(null);
  const status = statusQ.data ?? null;
  const runtime = runtimeQ.data ?? null;
  const platforms = platformsQ.data ?? [];
  const statusErr = statusQ.error
    ? errMessage(statusQ.error)
    : runtimeQ.error
      ? errMessage(runtimeQ.error)
      : null;
  const platErr = platformsQ.error ? errMessage(platformsQ.error) : null;

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
      await queryClient.invalidateQueries({ queryKey: qk.mcpPlatforms() });
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

      {runtimeQ.isPending ? (
        <Skeleton lines={2} />
      ) : (
        <>
          {running ? (
            <div className="mcp-status">
              <span className="dot-on" /> {t("mcp.httpRunning")}
              {status && (
                <>
                  {" "}
                  · <code>{status.url}</code>
                </>
              )}
              <InfoTip label="Claude Code · Cursor · VS Code" />
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
            <InfoTip label="Claude Desktop · Codex" />
            {!bridge && runtime?.error ? ` — ${runtime.error}` : ""}
          </div>
        </>
      )}

      <h3>{t("mcp.connectAgent")}</h3>
      {platErr ? (
        <div className="error">{t("mcp.platformDetectFailed", { error: platErr })}</div>
      ) : platformsQ.isPending ? (
        <Skeleton lines={3} />
      ) : platforms.length === 0 ? (
        <div className="muted">{t("mcp.noPlatforms")}</div>
      ) : (
        <div className="mcp-platforms">
          {platforms.map((p) => (
            <div className="plat" key={p.id}>
              <div className="plat-row">
                <div className="plat-info">
                  <strong>{p.name}</strong>
                  {p.connected && (
                    <span
                      className="badge status-ok icon-only-badge"
                      title={t("mcp.connected")}
                      aria-label={t("mcp.connected")}
                      role="img"
                    >
                      <Icon name="check" />
                    </span>
                  )}
                  {p.installed ? (
                    <InfoTip label={p.note} />
                  ) : (
                    <span
                      className="badge icon-only-badge"
                      title={t("mcp.notInstalled")}
                      aria-label={t("mcp.notInstalled")}
                      role="img"
                    >
                      <Icon name="circleSlash" />
                    </span>
                  )}
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
    </div>
  );
}
