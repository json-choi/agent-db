"use client";

import { authClient } from "../../lib/auth-client";

export function SignOutButton() {
  return (
    <button
      className="signout"
      type="button"
      onClick={() => authClient.signOut({ fetchOptions: { onSuccess: () => location.assign("/auth/sign-in") } })}
    >
      로그아웃 ↗
    </button>
  );
}
