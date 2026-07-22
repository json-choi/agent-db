"use client";

import { useState } from "react";
import { authClient } from "../../../lib/auth-client";

export function AcceptInvitation({ invitationId }: { invitationId: string }) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

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

  return (
    <>
      <button className="primary-button" type="button" onClick={accept} disabled={pending}>
        {pending ? "수락 중…" : "워크스페이스 참여"}<span>→</span>
      </button>
      {error ? <div className="error-banner">{error}</div> : null}
    </>
  );
}
