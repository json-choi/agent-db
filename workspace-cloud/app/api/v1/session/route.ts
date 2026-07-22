import { jsonError } from "../../../../lib/http";
import { auth } from "../../../../lib/auth";

export async function GET(request: Request) {
  const session = await auth.api.getSession({ headers: request.headers });
  if (!session) return jsonError("Unauthorized", 401);
  return Response.json({
    user: { id: session.user.id, email: session.user.email, displayName: session.user.name },
    session: { id: session.session.id, activeWorkspaceId: session.session.activeOrganizationId },
  });
}
