// Compact active-workspace control for the database explorer. Workspace changes clear
// cached resource reads before the shell reloads the newly selected local scope.
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  beginWorkspaceLogin,
  pollWorkspaceLogin,
  refreshWorkspaceMemberships,
  setActiveWorkspace,
} from "../ipc/commands";
import type { WorkspaceAuthState, WorkspaceLoginPoll } from "../ipc/types";
import { errMessage } from "../ipc/types";
import { useI18n } from "../lib/i18n";
import { resetWorkspaceResourceQueries } from "../lib/queryClient";
import { qk, workspaceAuthStateQuery, workspaceContextQuery } from "../lib/queries";
import { shouldRevalidateWorkspaceAuth } from "../lib/workspaceAuthLifecycle";
import { Icon } from "./Icon";
import { useToast } from "./Toast";
import "./WorkspaceSwitcher.css";

export default function WorkspaceSwitcher({
  onChanged,
  onNew,
}: {
  onChanged: () => void | Promise<void>;
  onNew: () => void;
}) {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const context = useQuery(workspaceContextQuery());
  const auth = useQuery(workspaceAuthStateQuery());
  const [switching, setSwitching] = useState(false);
  const [loginPhase, setLoginPhase] = useState<"idle" | "starting" | "waiting">("idle");
  const loginAttempt = useRef(0);
  const pendingLogin = useRef<{ attempt: number; deviceCode: string } | null>(null);
  const pollInFlight = useRef<Promise<WorkspaceLoginPoll> | null>(null);
  const membershipRefreshInFlight = useRef<Promise<void> | null>(null);
  const browserWasActive = useRef(false);
  const focusReturnHandler = useRef<() => void>(() => undefined);
  const membershipRefreshHandler = useRef<() => void>(() => undefined);

  useEffect(() => {
    const onBlur = () => {
      if (pendingLogin.current) browserWasActive.current = true;
    };
    const onFocus = () => {
      if (!pendingLogin.current) {
        membershipRefreshHandler.current();
        return;
      }
      if (!browserWasActive.current) return;
      browserWasActive.current = false;
      focusReturnHandler.current();
    };
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
      loginAttempt.current += 1;
      pendingLogin.current = null;
    };
  }, []);

  useEffect(() => {
    if (!auth.data?.authenticated) return;
    void queryClient.invalidateQueries({ queryKey: qk.workspaceContext() });
  }, [auth.data?.authenticated, queryClient]);

  async function wait(ms: number) {
    await new Promise<void>((resolve) => window.setTimeout(resolve, ms));
  }

  async function pollOnce(deviceCode: string) {
    if (pollInFlight.current) return pollInFlight.current;
    const request = pollWorkspaceLogin(deviceCode).finally(() => {
      if (pollInFlight.current === request) pollInFlight.current = null;
    });
    pollInFlight.current = request;
    return request;
  }

  function cancelLogin() {
    if (!pendingLogin.current) return;
    loginAttempt.current += 1;
    pendingLogin.current = null;
    setLoginPhase("idle");
    toast(t("workspace.loginCanceled"));
  }

  function handlePollResult(result: WorkspaceLoginPoll, attempt: number) {
    if (pendingLogin.current?.attempt !== attempt) return true;
    if (result.status === "signedIn" && result.user) {
      pendingLogin.current = null;
      const state: WorkspaceAuthState = { authenticated: true, user: result.user };
      queryClient.setQueryData(qk.workspaceAuth(), state);
      void queryClient.invalidateQueries({ queryKey: qk.workspaceContext() });
      setLoginPhase("idle");
      toast(t("workspace.loginComplete", { name: result.user.displayName }), "success");
      return true;
    }
    if (result.status === "denied" || result.status === "expired") {
      pendingLogin.current = null;
      setLoginPhase("idle");
      toast(
        t(result.status === "denied" ? "workspace.loginDenied" : "workspace.loginExpired"),
        "error",
      );
      return true;
    }
    return false;
  }

  async function checkAfterBrowserReturn() {
    const pending = pendingLogin.current;
    if (!pending) return;
    try {
      // Give the approval request a brief moment to commit before treating a returned
      // focus with a still-pending code as a closed/cancelled browser flow.
      await wait(350);
      let result = await pollOnce(pending.deviceCode);
      if (result.status === "slowDown") {
        await wait(5_250);
        if (pendingLogin.current?.attempt !== pending.attempt) return;
        result = await pollOnce(pending.deviceCode);
      }
      if (handlePollResult(result, pending.attempt)) return;
      cancelLogin();
    } catch {
      // Returning from the browser is an explicit local cancellation boundary even
      // when the network cannot confirm the still-pending server code.
      cancelLogin();
    }
  }

  focusReturnHandler.current = () => void checkAfterBrowserReturn();
  membershipRefreshHandler.current = () => {
    if (!auth.data?.authenticated || membershipRefreshInFlight.current) return;
    const revalidateAuth = shouldRevalidateWorkspaceAuth(
      true,
      auth.dataUpdatedAt,
      auth.isFetching,
    );
    const request = (revalidateAuth
      ? auth.refetch().then(() => undefined)
      : refreshWorkspaceMemberships().then(() => undefined)
    )
      .then(() => queryClient.invalidateQueries({ queryKey: qk.workspaceContext() }))
      .catch(async () => {
        // A membership 401 also invalidates the hosted session. Confirm that state
        // silently so expired team scopes disappear without turning the button into
        // a foreground loading indicator.
        await auth.refetch().catch(() => undefined);
        await queryClient.invalidateQueries({ queryKey: qk.workspaceContext() });
      })
      .finally(() => {
        if (membershipRefreshInFlight.current === request) {
          membershipRefreshInFlight.current = null;
        }
      });
    membershipRefreshInFlight.current = request;
  };

  async function login() {
    if (auth.data?.authenticated || loginPhase !== "idle") return;
    const attempt = ++loginAttempt.current;
    setLoginPhase("starting");
    try {
      const authorization = await beginWorkspaceLogin();
      pendingLogin.current = { attempt, deviceCode: authorization.deviceCode };
      browserWasActive.current = false;
      await openUrl(authorization.verificationUriComplete);
      if (loginAttempt.current !== attempt) return;
      setLoginPhase("waiting");
      const expiresAt = Date.now() + authorization.expiresIn * 1000;
      let pollInterval = Math.max(authorization.interval, 1) * 1000;

      while (Date.now() < expiresAt) {
        await wait(pollInterval);
        if (loginAttempt.current !== attempt) return;
        const result = await pollOnce(authorization.deviceCode);
        if (result.status === "pending") continue;
        if (result.status === "slowDown") {
          pollInterval += 5_000;
          continue;
        }
        if (handlePollResult(result, attempt)) return;
      }
      pendingLogin.current = null;
      toast(t("workspace.loginExpired"), "error");
    } catch (error) {
      pendingLogin.current = null;
      toast(t("workspace.loginFailed", { error: errMessage(error) }), "error");
    } finally {
      if (loginAttempt.current === attempt) setLoginPhase("idle");
    }
  }

  const authKnown = auth.data !== undefined;
  const loginLabel = !authKnown
    ? t("workspace.loginChecking")
    : auth.data.authenticated
      ? t("workspace.loginCompleteShort")
      : loginPhase === "starting"
        ? t("workspace.loginStarting")
        : loginPhase === "waiting"
          ? t("workspace.loginCancel")
          : t("workspace.login");

  async function changeWorkspace(id: string) {
    if (!context.data?.feature.enabled) return;
    const { active } = context.data;
    if (id === active.id || switching) return;
    setSwitching(true);
    try {
      await setActiveWorkspace(id);
      await resetWorkspaceResourceQueries(queryClient);
      await queryClient.invalidateQueries({
        queryKey: qk.workspaceContext(),
        refetchType: "none",
      });
      await queryClient.fetchQuery(workspaceContextQuery());
      await onChanged();
    } catch (error) {
      toast(t("workspace.switchFailed", { error: errMessage(error) }), "error");
    } finally {
      setSwitching(false);
    }
  }

  return (
    <div className="workspace-switcher" data-tauri-drag-region="deep">
      <div className="workspace-switcher-head">
        <span className="workspace-switcher-label">{t("workspace.label")}</span>
        <div className="workspace-switcher-actions">
          <button
            type="button"
            className="btn small workspace-login-button"
            onClick={() => (loginPhase === "waiting" ? cancelLogin() : void login())}
            disabled={!authKnown || loginPhase === "starting" || auth.data.authenticated}
            data-checking={!authKnown || undefined}
            data-authenticated={auth.data?.authenticated || undefined}
            title={
              !authKnown
                ? t("workspace.loginChecking")
                : auth.data.user
                ? `${auth.data.user.displayName} · ${auth.data.user.email}`
                : loginPhase === "waiting"
                  ? t("workspace.loginPending")
                  : undefined
            }
            aria-live="polite"
          >
            {loginLabel}
          </button>
          <button
            type="button"
            className="btn small workspace-add-button"
            onClick={onNew}
            title={t("connections.new")}
            aria-label={t("connections.new")}
          >
            <Icon name="plus" />
          </button>
        </div>
      </div>
      {context.isLoading ? (
        <div className="workspace-select-skeleton" aria-hidden="true" />
      ) : context.data?.feature.enabled ? (
        <div className="workspace-select-wrap">
          <select
            value={context.data.active.id}
            onChange={(event) => void changeWorkspace(event.target.value)}
            disabled={switching}
            aria-label={t("workspace.select")}
          >
            {context.data.workspaces.map((workspace) => (
              <option key={workspace.id} value={workspace.id}>
                {workspace.kind === "personal"
                  ? `${t("workspace.personalName")} · ${t("workspace.localOnly")}`
                  : workspace.name}
              </option>
            ))}
          </select>
          <Icon name="chevronDown" />
        </div>
      ) : null}
    </div>
  );
}
