// Verified invitation acceptance with a multi-account escape hatch. Users can switch
// to an already signed-in identity or add the Google account that received the invite.
"use client";

import { useState } from "react";
import { authClient } from "../../../lib/auth-client";
import { useDeviceAccounts } from "../../../lib/useDeviceAccounts";

export function AcceptInvitation({
  invitationId,
  currentUserId,
}: {
  invitationId: string;
  currentUserId: string;
}) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const { accounts, error: accountError } = useDeviceAccounts();

  async function accept() {
    setPending(true);
    setError("");
    const result = await authClient.organization.acceptInvitation({ invitationId });
    if (result.error) {
      setPending(false);
      setError(result.error.message ?? "초대를 수락하지 못했습니다.");
      return;
    }
    window.location.assign("/settings");
  }

  async function switchAccount(sessionToken: string) {
    setPending(true);
    const result = await authClient.multiSession.setActive({ sessionToken });
    if (result.error) {
      setPending(false);
      setError(result.error.message ?? "계정을 전환하지 못했습니다.");
      return;
    }
    window.location.reload();
  }

  return (
    <>
      <button className="primary-button" type="button" onClick={accept} disabled={pending}>
        {pending ? "수락 중…" : "워크스페이스 참여"}<span>→</span>
      </button>
      {accounts.length > 1 ? (
        <div className="invitation-account-list">
          <small>다른 계정으로 받은 초대인가요?</small>
          {accounts.filter((account) => account.user.id !== currentUserId).map((account) => (
            <button type="button" key={account.user.id} onClick={() => void switchAccount(account.sessions[0].session.token)} disabled={pending}>
              <span>{account.user.name}</span><small>{account.user.email}</small>
            </button>
          ))}
        </div>
      ) : null}
      <a className="secondary-button invitation-add-account" href={`/auth/sign-in?returnTo=${encodeURIComponent(`/accept-invitation/${invitationId}`)}`}>
        다른 Google 계정 추가
      </a>
      {error ? <div className="error-banner">{error}</div> : null}
      {!error && accountError ? <div className="error-banner">{accountError}</div> : null}
    </>
  );
}
