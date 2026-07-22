// Optional Resend transport for Better Auth organization invitations. Deployments
// without provider credentials retain the verified, email-bound copy-link workflow.
import "server-only";

interface InvitationEmailData {
  id: string;
  email: string;
  inviter: { user: { name: string; email: string } };
  organization: { name: string };
}

/**
 * Deliver a plain-text invitation when Resend is configured. Link-copy remains an
 * intentional fallback for self-hosted deployments without an email provider.
 * Recipient addresses and action-capable invitation URLs are never logged.
 */
export async function sendWorkspaceInvitation(
  data: InvitationEmailData,
  appOrigin: string,
) {
  const apiKey = process.env.RESEND_API_KEY?.trim();
  const from = process.env.WORKSPACE_INVITATION_FROM?.trim();
  if (!apiKey || !from) return;

  const inviteUrl = `${appOrigin}/accept-invitation/${encodeURIComponent(data.id)}`;
  const response = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
      "idempotency-key": `workspace-invitation-${data.id}`,
    },
    body: JSON.stringify({
      from,
      to: [data.email],
      subject: `${data.organization.name} 워크스페이스 초대`,
      text: [
        `${data.inviter.user.name} (${data.inviter.user.email})님이`,
        `${data.organization.name} 워크스페이스에 초대했습니다.`,
        "",
        `초대 수락: ${inviteUrl}`,
        "",
        "이 링크는 초대받은 Google 이메일로 로그인한 경우에만 사용할 수 있습니다.",
      ].join("\n"),
    }),
  });
  if (!response.ok) {
    throw new Error(`Invitation email delivery failed with status ${response.status}`);
  }
}
