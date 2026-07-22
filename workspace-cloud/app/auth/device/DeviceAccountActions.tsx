// Account selector for RFC 8628 approval. It prevents “add account” on desktop from
// silently re-authorizing whichever browser identity happened to be active.
"use client";

import { useState } from "react";
import { authClient } from "../../../lib/auth-client";
import { useDeviceAccounts } from "../../../lib/useDeviceAccounts";

export function DeviceAccountActions({
  currentUserId,
  userCode,
}: {
  currentUserId: string;
  userCode: string;
}) {
  const { accounts, error: accountError } = useDeviceAccounts();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const returnTo = `/auth/device?user_code=${encodeURIComponent(userCode)}`;

  async function switchAccount(sessionToken: string) {
    setPending(true);
    setError("");
    const result = await authClient.multiSession.setActive({ sessionToken });
    if (result.error) {
      setPending(false);
      setError(result.error.message ?? "계정을 전환하지 못했습니다.");
      return;
    }
    location.assign(returnTo);
  }

  return (
    <div className="device-account-actions ds-control-row">
      {accounts.filter((account) => account.user.id !== currentUserId).map((account) => (
        <button
          type="button"
          key={account.user.id}
          onClick={() => void switchAccount(account.sessions[0].session.token)}
          disabled={pending}
        >
          <span>{account.user.name}</span><small>{account.user.email}</small>
        </button>
      ))}
      <a href={`/auth/sign-in?returnTo=${encodeURIComponent(returnTo)}`}>＋ 다른 Google 계정으로 승인</a>
      {error ? <small className="form-error" role="alert">{error}</small> : null}
      {!error && accountError ? (
        <small className="form-error" role="alert">{accountError}</small>
      ) : null}
    </div>
  );
}
