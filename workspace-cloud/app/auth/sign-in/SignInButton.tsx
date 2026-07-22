"use client";

import { useState } from "react";
import { authClient } from "../../../lib/auth-client";

export function SignInButton({ returnTo }: { returnTo: string }) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  async function signIn() {
    setPending(true);
    setError("");
    const result = await authClient.signIn.social({
      provider: "google",
      callbackURL: returnTo,
      errorCallbackURL: "/auth/sign-in?error=oauth_failed",
    });
    if (result.error) {
      setPending(false);
      setError(result.error.message ?? "Google 로그인을 시작하지 못했습니다.");
    }
  }

  return (
    <>
      <button className="google-button" type="button" onClick={signIn} disabled={pending}>
        <span className="google-g">G</span>
        <span>{pending ? "Google로 이동 중…" : "Google로 계속"}</span>
        <span aria-hidden="true">↗</span>
      </button>
      {error ? <div className="error-banner">{error}</div> : null}
    </>
  );
}
