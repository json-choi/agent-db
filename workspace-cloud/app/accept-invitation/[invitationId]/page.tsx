import { headers } from "next/headers";
import { redirect } from "next/navigation";
import { auth } from "../../../lib/auth";
import { Brand } from "../../components/Brand";
import { AcceptInvitation } from "./AcceptInvitation";

export const dynamic = "force-dynamic";

export default async function InvitationPage({
  params,
}: {
  params: Promise<{ invitationId: string }>;
}) {
  const { invitationId } = await params;
  const session = await auth.api.getSession({ headers: await headers() });
  if (!session) {
    redirect(`/auth/sign-in?returnTo=${encodeURIComponent(`/accept-invitation/${invitationId}`)}`);
  }
  return (
    <main className="single-shell">
      <Brand />
      <section className="device-card">
        <p className="eyebrow">VERIFIED INVITATION</p>
        <h1>워크스페이스 초대</h1>
        <p>{session.user.email} 계정으로 초대를 수락합니다. 초대 이메일과 로그인 이메일이 일치해야 합니다.</p>
        <AcceptInvitation invitationId={invitationId} />
      </section>
    </main>
  );
}
