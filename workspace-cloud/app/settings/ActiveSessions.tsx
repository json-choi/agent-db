// Current-account session inventory with explicit remote revocation. Session tokens
// are endpoint inputs only and never become DOM content.
"use client";

import { useEffect, useState } from "react";
import { authClient } from "../../lib/auth-client";

interface SessionItem {
  id: string;
  token: string;
  updatedAt: Date;
  userAgent?: string | null;
  ipAddress?: string | null;
}

export function ActiveSessions({ currentSessionId }: { currentSessionId: string }) {
  const [sessions, setSessions] = useState<SessionItem[]>([]);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState("");

  async function refresh() {
    const result = await authClient.listSessions();
    if (result.error) {
      setError(result.error.message ?? "세션을 불러오지 못했습니다.");
      return;
    }
    setSessions(result.data ?? []);
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function revoke(item: SessionItem) {
    if (pending) return;
    setPending(item.id);
    setError("");
    const result = await authClient.revokeSession({ token: item.token });
    if (result.error) {
      setError(result.error.message ?? "세션을 종료하지 못했습니다.");
      setPending(null);
      return;
    }
    await refresh();
    setPending(null);
  }

  return (
    <div className="device-table">
      {sessions.map((item) => {
        const current = item.id === currentSessionId;
        return (
          <div className="device-row" key={item.id}>
            <span className="device-icon">{item.userAgent?.includes("Mozilla") ? "◎" : "▣"}</span>
            <div>
              <strong>{item.userAgent?.includes("Mozilla") ? "Web browser" : "DopeDB desktop"}</strong>
              <small>{item.ipAddress ?? "protected session"}</small>
            </div>
            {current ? (
              <time>현재 세션</time>
            ) : (
              <button type="button" onClick={() => void revoke(item)} disabled={pending === item.id}>
                {pending === item.id ? "종료 중…" : "세션 종료"}
              </button>
            )}
          </div>
        );
      })}
      {sessions.length === 0 && !error ? <p className="device-empty">활성 세션을 확인하고 있습니다…</p> : null}
      {error ? <p className="device-error" role="alert">{error}</p> : null}
    </div>
  );
}
