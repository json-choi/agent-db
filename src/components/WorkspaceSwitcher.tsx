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
  signOutWorkspace,
  workspaceConsoleUrl,
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

export function WorkspaceAccount({ onSignedOut }: { onSignedOut: () => void | Promise<void> }) {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const auth = useQuery(workspaceAuthStateQuery());
  const [loginPhase, setLoginPhase] = useState<"idle" | "starting" | "waiting">("idle");
  const [loggingOut, setLoggingOut] = useState(false);
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

  async function logout() {
    if (loggingOut) return;
    loginAttempt.current += 1;
    pendingLogin.current = null;
    setLoginPhase("idle");
    setLoggingOut(true);
    try {
      const signedOut = await signOutWorkspace();
      queryClient.setQueryData(qk.workspaceAuth(), signedOut);
      await resetWorkspaceResourceQueries(queryClient);
      await queryClient.invalidateQueries({ queryKey: qk.workspaceContext() });
      await onSignedOut();
      toast(t("workspace.logoutComplete"), "success");
    } catch (error) {
      // The native command may already have removed the credential before a local
      // workspace-index error. Re-read identity so the UI never displays a stale user.
      await auth.refetch().catch(() => undefined);
      await queryClient.invalidateQueries({ queryKey: qk.workspaceContext() });
      toast(t("workspace.logoutFailed", { error: errMessage(error) }), "error");
    } finally {
      setLoggingOut(false);
    }
  }

  const authKnown = auth.data !== undefined;
  const loginLabel = !authKnown
    ? t("workspace.loginChecking")
    : loginPhase === "starting"
      ? t("workspace.loginStarting")
      : loginPhase === "waiting"
        ? t("workspace.loginCancel")
        : t("workspace.login");

  const user = auth.data?.authenticated ? auth.data.user : null;

  return (
    <div className="workspace-account" aria-live="polite">
      {!authKnown ? (
        <div className="workspace-account-skeleton" aria-label={loginLabel} />
      ) : user ? (
        <>
          <span className="workspace-account-avatar" aria-hidden="true">
            {(user.displayName || user.email).slice(0, 1).toUpperCase()}
          </span>
          <span className="workspace-account-copy">
            <strong>{user.displayName}</strong>
            <small>{user.email}</small>
          </span>
          <button
            type="button"
            className="workspace-account-action"
            onClick={() => void logout()}
            disabled={loggingOut}
            title={t(loggingOut ? "workspace.logoutPending" : "workspace.logout")}
            aria-label={t(loggingOut ? "workspace.logoutPending" : "workspace.logout")}
            aria-busy={loggingOut}
          >
            <Icon name="logOut" />
          </button>
        </>
      ) : (
        <button
          type="button"
          className="workspace-account-login"
          onClick={() => (loginPhase === "waiting" ? cancelLogin() : void login())}
          disabled={loginPhase === "starting"}
          title={loginPhase === "waiting" ? t("workspace.loginPending") : undefined}
        >
          {loginLabel}
        </button>
      )}
    </div>
  );
}

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
  const [switching, setSwitching] = useState(false);
  const [dashboardOpening, setDashboardOpening] = useState(false);

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

  async function openDashboard() {
    if (!context.data?.feature.enabled || dashboardOpening) return;
    setDashboardOpening(true);
    try {
      const { active } = context.data;
      const url = await workspaceConsoleUrl(active.kind === "team" ? active.id : undefined);
      await openUrl(url);
    } catch (error) {
      toast(t("workspace.dashboardOpenFailed", { error: errMessage(error) }), "error");
    } finally {
      setDashboardOpening(false);
    }
  }

  const dashboardLabel =
    context.data?.active.kind === "team"
      ? t("workspace.openDashboardFor", { name: context.data.active.name })
      : t("workspace.openDashboard");

  return (
    <div className="workspace-switcher" data-tauri-drag-region="deep">
      <div className="workspace-switcher-head">
        <span className="workspace-switcher-label">{t("workspace.label")}</span>
        <div className="workspace-switcher-actions ds-control-row">
          <button
            type="button"
            className="btn small workspace-add-button"
            onClick={onNew}
            title={t("connections.new")}
            aria-label={t("connections.new")}
          >
            <Icon name="plus" />
          </button>
          <button
            type="button"
            className="btn small workspace-dashboard-button"
            onClick={() => void openDashboard()}
            disabled={!context.data?.feature.enabled || dashboardOpening}
            title={dashboardLabel}
            aria-label={dashboardLabel}
            aria-busy={dashboardOpening}
          >
            <Icon name="externalLink" />
          </button>
        </div>
      </div>
      {context.isLoading ? (
        <div className="workspace-select-row ds-control-row">
          <div className="workspace-select-skeleton" aria-hidden="true" />
        </div>
      ) : context.data?.feature.enabled ? (
        <div className="workspace-select-row ds-control-row">
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
        </div>
      ) : null}
    </div>
  );
}
