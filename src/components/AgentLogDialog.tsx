// Secondary Agent activity surface: keeps MCP results, context, and audit details available
// without making the log viewer compete with the connection-scoped conversation tab.
import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import type { ConnectionProfile, Dashboard } from "../ipc/types";
import { useI18n } from "../lib/i18n";
import AgentResultView from "./AgentResultView";
import EngineMark from "./EngineMark";
import { Icon } from "./Icon";
import "./AgentLogDialog.css";

export default function AgentLogDialog({
  connection,
  onDashboardSaved,
  onClose,
}: {
  connection: ConnectionProfile;
  onDashboardSaved: (dashboard: Dashboard) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const dialogRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = Array.from(
        dialogRef.current?.querySelectorAll<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ) ?? [],
      );
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      trigger?.focus?.();
    };
  }, [onClose]);

  return createPortal(
    <div className="agent-log-overlay" role="presentation" onClick={onClose}>
      <div
        ref={dialogRef}
        className="agent-log-dialog ds-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="agent-log-title"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="agent-log-head">
          <div className="ds-title-line">
            <EngineMark engine={connection.engine} />
            <strong id="agent-log-title">
              {t("agentChat.logsFor", { name: connection.name || t("app.unnamed") })}
            </strong>
            {connection.env && (
              <span className={`env-chip env-${connection.env}`}>{connection.env}</span>
            )}
          </div>
          <button
            ref={closeRef}
            type="button"
            className="btn small"
            onClick={onClose}
            title={t("common.close")}
            aria-label={t("common.close")}
          >
            <Icon name="close" />
          </button>
        </header>
        <div className="agent-log-body">
          <AgentResultView
            compact
            connectionId={connection.id}
            onDashboardSaved={(dashboard) => {
              onClose();
              onDashboardSaved(dashboard);
            }}
          />
        </div>
      </div>
    </div>,
    document.body,
  );
}
