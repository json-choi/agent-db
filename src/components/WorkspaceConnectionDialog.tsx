// Secure workspace connection flow: publishes only a redacted local template or
// binds a member-local credential to a synchronized template.
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  bindWorkspaceConnectionCredentials,
  copyConnectionToWorkspace,
} from "../ipc/commands";
import type { ConnectionProfile } from "../ipc/types";
import { errDetails } from "../ipc/types";
import { useI18n } from "../lib/i18n";
import { qk, workspaceAuthStateQuery, workspaceContextQuery } from "../lib/queries";
import {
  buildWorkspaceChoiceGroups,
  canManageWorkspaceConnections,
  parseWorkspaceChoice,
} from "../lib/workspaceAccounts";
import { useToast } from "./Toast";
import "./WorkspaceConnectionDialog.css";

export default function WorkspaceConnectionDialog({
  connection,
  mode,
  onBound,
  onClose,
}: {
  connection: ConnectionProfile;
  mode: "copy" | "credentials";
  onBound: (connection: ConnectionProfile) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const context = useQuery(workspaceContextQuery());
  const auth = useQuery(workspaceAuthStateQuery());
  const targetGroups = useMemo(
    () => buildWorkspaceChoiceGroups(
      auth.data,
      context.data?.workspaces ?? [],
      t("workspace.localOnly"),
    )
      .map((group) => ({
        ...group,
        choices: group.choices.filter((choice) =>
          choice.workspace.kind === "team"
          && canManageWorkspaceConnections(choice.role)
          && !(
            choice.workspace.id === context.data?.active.id
            && choice.accountUserId === auth.data?.user?.id
          ),
        ),
      }))
      .filter((group) => group.choices.length > 0),
    [auth.data, context.data, t],
  );
  const targets = targetGroups.flatMap((group) => group.choices);
  const [targetValue, setTargetValue] = useState("");
  const [username, setUsername] = useState(connection.username);
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const dialogRef = useRef<HTMLFormElement>(null);
  const initialFocusRef = useRef<HTMLElement | null>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const selectedTargetValue = targetValue || targets[0]?.value || "";

  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    const focusTarget = initialFocusRef.current;
    if (focusTarget instanceof HTMLSelectElement && focusTarget.disabled) {
      cancelRef.current?.focus();
    } else {
      focusTarget?.focus();
    }
    return () => trigger?.focus?.();
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !pending) {
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
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onClose, pending]);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    setPending(true);
    setError("");
    try {
      if (mode === "copy") {
        const target = parseWorkspaceChoice(selectedTargetValue);
        if (!target?.accountUserId) throw new Error("A workspace account is required.");
        await copyConnectionToWorkspace(
          connection.id,
          target.workspaceId,
          target.accountUserId,
        );
        await queryClient.invalidateQueries({ queryKey: qk.workspaceContext() });
        toast(t("workspace.connectionCopied"));
      } else {
        const bound = await bindWorkspaceConnectionCredentials(
          connection.id,
          username,
          password,
        );
        onBound(bound);
        toast(t("workspace.credentialsBound"));
      }
      onClose();
    } catch (caught) {
      const details = errDetails(caught);
      const serviceUnavailable =
        details.kind === "network" && details.message.includes("404 Not Found");
      setError(
        serviceUnavailable
          ? t("workspace.connectionServiceUnavailable")
          : details.message,
      );
    } finally {
      setPending(false);
    }
  }

  return createPortal(
    <div
      className="workspace-connection-overlay"
      role="presentation"
      onClick={() => {
        if (!pending) onClose();
      }}
    >
      <form
        ref={dialogRef}
        className="workspace-connection-dialog ds-panel form"
        role="dialog"
        aria-modal="true"
        aria-labelledby="workspace-connection-title"
        aria-describedby="workspace-connection-description"
        aria-busy={pending}
        onSubmit={submit}
        onClick={(event) => event.stopPropagation()}
      >
        <header className="form-head">
          <h2 id="workspace-connection-title">
            {mode === "copy"
              ? t("workspace.copyConnection", { name: connection.name })
              : t("workspace.bindCredentials", { name: connection.name })}
          </h2>
        </header>
        {mode === "copy" ? (
          <>
            <p id="workspace-connection-description" className="workspace-connection-description">
              {t("workspace.copySecurityNote")}
            </p>
            <label>
              {t("workspace.targetWorkspace")}
              <select
                ref={(node) => {
                  initialFocusRef.current = node;
                }}
                value={selectedTargetValue}
                onChange={(event) => setTargetValue(event.target.value)}
                disabled={pending || targets.length === 0}
              >
                {targetGroups.map((group) => (
                  <optgroup key={group.key} label={group.label}>
                    {group.choices.map((choice) => (
                      <option value={choice.value} key={choice.value}>
                        {choice.workspace.name}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
            </label>
            {targets.length === 0 ? <div className="error">{t("workspace.noManageableWorkspace")}</div> : null}
          </>
        ) : (
          <>
            <p id="workspace-connection-description" className="workspace-connection-description">
              {t("workspace.credentialsSecurityNote")}
            </p>
            <label>
              {t("workspace.username")}
              <input
                ref={(node) => {
                  initialFocusRef.current = node;
                }}
                value={username}
                onChange={(event) => setUsername(event.target.value)}
                autoComplete="username"
              />
            </label>
            <label>
              {t("connections.password")}
              <input type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="current-password" required />
            </label>
          </>
        )}
        {error ? <div className="form-msg error workspace-connection-error" role="alert">{error}</div> : null}
        <footer className="form-actions ds-control-row">
          <button ref={cancelRef} className="btn" type="button" onClick={onClose} disabled={pending}>{t("common.cancel")}</button>
          <button className="btn primary" type="submit" disabled={pending || (mode === "copy" && !selectedTargetValue)}>
            {pending ? t("mcp.working") : mode === "copy" ? t("workspace.copy") : t("workspace.bind")}
          </button>
        </footer>
      </form>
    </div>,
    document.body,
  );
}
