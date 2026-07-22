import { Brand } from "../../components/Brand";
import { safeReturnTo } from "../../../lib/http";
import { SignInButton } from "./SignInButton";

const messages: Record<string, string> = {
  oauth_state_missing: "로그인 요청이 만료되었습니다. 다시 시도해 주세요.",
  oauth_state_invalid: "로그인 요청을 확인할 수 없습니다. 다시 시작해 주세요.",
  email_not_verified: "확인된 Google 이메일이 필요합니다.",
  oauth_failed: "Google 로그인을 완료하지 못했습니다. 잠시 후 다시 시도해 주세요.",
};

export default async function SignInPage({
  searchParams,
}: {
  searchParams: Promise<{ returnTo?: string; error?: string }>;
}) {
  const params = await searchParams;
  const returnTo = safeReturnTo(params.returnTo ?? null);
  return (
    <main className="auth-shell">
      <div className="auth-top"><Brand /><span className="system-state"><i /> Identity gateway online</span></div>
      <section className="auth-grid">
        <div className="auth-context">
          <p className="eyebrow">CONTROL PLANE / 01</p>
          <h1>팀의 데이터 작업을<br />한 경계 안에서.</h1>
          <p className="lede">워크스페이스는 연결 정보, 권한, 대시보드와 변경 이력을 팀 단위로 분리합니다.</p>
          <div className="signal-list">
            <span>Better Auth</span><span>RFC 8628 device login</span><span>Drizzle ORM</span>
          </div>
        </div>
        <div className="auth-panel">
          <div className="panel-index">AUTH / GOOGLE</div>
          <h2>워크스페이스에 로그인</h2>
          <p>Google 계정으로 본인을 확인합니다. Google 액세스 토큰은 DopeDB에 보관하지 않습니다.</p>
          {params.error ? <div className="error-banner">{messages[params.error] ?? "로그인에 실패했습니다."}</div> : null}
          <SignInButton returnTo={returnTo} />
          <p className="legal">계속하면 조직의 워크스페이스 정책과 감사 기록 적용에 동의합니다.</p>
        </div>
      </section>
      <footer className="auth-footer"><span>DopeDB cloud control plane</span><span>Seoul · Virginia</span></footer>
    </main>
  );
}
