// Unified Better Auth account switcher for the web console. Account-session tokens
// remain closure data and are never rendered, logged, or persisted by this component.
"use client";

import { useEffect, useRef, useState } from "react";
import { authClient } from "../../lib/auth-client";
import { useDeviceAccounts } from "../../lib/useDeviceAccounts";

export function AccountSwitcher({
  currentUser,
}: {
  currentUser: { id: string; name: string; email: string };
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const { accounts, sessions, error, setError } = useDeviceAccounts();
  const currentUserId = currentUser.id;
  const current = accounts.find((account) => account.user.id === currentUserId);

  useEffect(() => {
    if (!open) return;
    const closeOutside = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", closeOutside);
    window.addEventListener("keydown", closeOnEscape);
    return () => {
      window.removeEventListener("mousedown", closeOutside);
      window.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  async function activate(sessionToken: string, userId: string) {
    if (userId === currentUserId || pending) return;
    setPending(userId);
    setError("");
    const result = await authClient.multiSession.setActive({ sessionToken });
    if (result.error) {
      setPending(null);
      setError(result.error.message ?? "계정을 전환하지 못했습니다.");
      return;
    }
    location.assign("/settings");
  }

  async function revokeAccount(userId: string) {
    if (pending) return;
    setPending(userId);
    setError("");
    const target = accounts.find((account) => account.user.id === userId);
    if (!target) {
      setPending(null);
      return;
    }
    if (userId === currentUserId) {
      const fallback = accounts.find((account) => account.user.id !== userId);
      if (fallback) {
        const switched = await authClient.multiSession.setActive({
          sessionToken: fallback.sessions[0].session.token,
        });
        if (switched.error) {
          setPending(null);
          setError(switched.error.message ?? "다른 계정으로 전환하지 못했습니다.");
          return;
        }
      }
    }
    for (const item of target.sessions) {
      const result = await authClient.multiSession.revoke({
        sessionToken: item.session.token,
      });
      if (result.error) {
        setPending(null);
        setError(result.error.message ?? "계정 세션을 종료하지 못했습니다.");
        return;
      }
    }
    location.assign(accounts.length > 1 ? "/settings" : "/auth/sign-in");
  }

  async function revokeAll() {
    if (pending) return;
    setPending("all");
    setError("");
    const inactive = sessions.filter((item) => item.user.id !== currentUserId);
    for (const item of inactive) {
      const result = await authClient.multiSession.revoke({
        sessionToken: item.session.token,
      });
      if (result.error) {
        setPending(null);
        setError(result.error.message ?? "일부 계정 세션을 종료하지 못했습니다.");
        return;
      }
    }
    const result = await authClient.signOut();
    if (result.error) {
      setPending(null);
      setError(result.error.message ?? "현재 계정에서 로그아웃하지 못했습니다.");
      return;
    }
    location.assign("/auth/sign-in");
  }

  return (
    <div className="account-switcher" ref={rootRef}>
      <button
        className="account-switcher-trigger"
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        aria-haspopup="menu"
      >
        <span>{(current?.user.name ?? currentUser.name).slice(0, 1).toUpperCase()}</span>
        <strong>{current?.user.email ?? currentUser.email}</strong>
        <i aria-hidden="true">⌃</i>
      </button>
      {open ? (
        <div className="account-switcher-menu" role="menu">
          {accounts.map((account) => (
            <div className="account-switcher-row" key={account.user.id}>
              <button
                type="button"
                role="menuitemradio"
                aria-checked={account.user.id === currentUserId}
                onClick={() => void activate(account.sessions[0].session.token, account.user.id)}
                disabled={pending !== null}
              >
                <span>{account.user.name.slice(0, 1).toUpperCase()}</span>
                <div><strong>{account.user.name}</strong><small>{account.user.email}</small></div>
                <i>{account.user.id === currentUserId ? "✓" : ""}</i>
              </button>
              <button
                className="account-revoke"
                type="button"
                role="menuitem"
                onClick={() => void revokeAccount(account.user.id)}
                disabled={pending !== null}
                aria-label={`${account.user.email} 로그아웃`}
              >
                ×
              </button>
            </div>
          ))}
          <a role="menuitem" href="/auth/sign-in?returnTo=%2Fsettings">＋ 다른 계정 추가</a>
          {accounts.length > 1 ? (
            <button className="account-signout-all" role="menuitem" type="button" onClick={() => void revokeAll()} disabled={pending !== null}>
              모든 계정에서 로그아웃
            </button>
          ) : null}
          {error ? <p role="alert">{error}</p> : null}
        </div>
      ) : null}
    </div>
  );
}
