// Shared Better Auth multi-session projection. Raw session tokens stay in client
// memory and are used only as endpoint inputs; callers must never render or log them.
"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { authClient } from "./auth-client";

interface DeviceSession {
  session: {
    id: string;
    token: string;
    updatedAt: Date;
  };
  user: {
    id: string;
    name: string;
    email: string;
  };
}

export function useDeviceAccounts() {
  const [sessions, setSessions] = useState<DeviceSession[]>([]);
  const [error, setError] = useState("");
  const refresh = useCallback(async () => {
    const result = await authClient.multiSession.listDeviceSessions();
    if (result.error) {
      setError(result.error.message ?? "계정 목록을 불러오지 못했습니다.");
      return;
    }
    setError("");
    setSessions(result.data ?? []);
  }, []);
  useEffect(() => {
    void refresh();
  }, [refresh]);
  const accounts = useMemo(() => {
    const grouped = new Map<string, DeviceSession[]>();
    for (const item of sessions) {
      const current = grouped.get(item.user.id) ?? [];
      current.push(item);
      grouped.set(item.user.id, current);
    }
    return [...grouped.values()].map((items) => ({
      user: items[0].user,
      sessions: items.sort(
        (a, b) => new Date(b.session.updatedAt).getTime() - new Date(a.session.updatedAt).getTime(),
      ),
    }));
  }, [sessions]);
  return { accounts, sessions, error, setError, refresh };
}
