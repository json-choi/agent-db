// Browser half of the RFC 8628 device flow. It verifies the short-lived code on the
// server and requires an explicit approve or deny action from the signed-in user.
import { headers } from "next/headers";
import { Brand } from "../../components/Brand";
import { auth } from "../../../lib/auth";
import { DeviceApproval } from "./DeviceApproval";
import { DeviceAccountActions } from "./DeviceAccountActions";
import { SignInButton } from "../sign-in/SignInButton";

export default async function DevicePage({
  searchParams,
}: {
  searchParams: Promise<{ user_code?: string; error?: string }>;
}) {
  const params = await searchParams;
  const userCode = params.user_code?.trim() ?? "";
  const valid = /^[A-Z2-9-]{6,20}$/i.test(userCode);
  const requestHeaders = await headers();
  const session = await auth.api.getSession({ headers: requestHeaders });
  let verificationError = params.error ? "올바르지 않은 승인 요청입니다." : "";

  if (valid) {
    try {
      await auth.api.deviceVerify({ query: { user_code: userCode }, headers: requestHeaders });
    } catch {
      verificationError = "승인 코드가 올바르지 않거나 만료되었습니다.";
    }
  }

  const returnTo = `/auth/device?user_code=${encodeURIComponent(userCode)}`;
  return (
    <main className="single-shell">
      <Brand />
      <section className="device-card">
        <div className="device-orbit"><span>↗</span></div>
        <p className="eyebrow">BETTER AUTH / RFC 8628</p>
        <h1>이 기기를 연결할까요?</h1>
        <p>Better Auth의 표준 Device Authorization 흐름으로 데스크톱 앱에 별도 세션을 발급합니다.</p>
        {!valid || verificationError ? (
          <div className="error-banner">{verificationError || "올바른 승인 코드가 필요합니다."}</div>
        ) : session ? (
          <div>
            <div className="signed-user"><span>{session.user.name.slice(0, 1).toUpperCase()}</span><div><strong>{session.user.name}</strong><small>{session.user.email}</small></div></div>
            <DeviceAccountActions currentUserId={session.user.id} userCode={userCode} />
            <DeviceApproval userCode={userCode} />
          </div>
        ) : (
          <SignInButton returnTo={returnTo} />
        )}
        <small className="expiry-note">승인 코드는 생성 후 10분 동안만 유효합니다.</small>
      </section>
    </main>
  );
}
