// Server-side workspace authorization. Every resource request resolves the session
// and membership from the database and fails closed; client role claims are ignored.
import "server-only";

import { and, eq, isNull } from "drizzle-orm";
import { db } from "./db";
import { auth } from "./auth";
import { member } from "./schema";
import {
  accessModeForRole,
  hasWorkspaceCapability,
  isWorkspaceRole,
  type WorkspaceCapability,
} from "./workspace-permissions";

export type { WorkspaceRoleName } from "./workspace-permissions";

export async function authorizeWorkspace(
  request: Request,
  organizationId: string,
  capability: WorkspaceCapability,
) {
  const session = await auth.api.getSession({ headers: request.headers });
  if (!session) return { ok: false as const, status: 401, error: "Unauthorized" };

  const membership = await db.query.member.findFirst({
    where: and(
      eq(member.organizationId, organizationId),
      eq(member.userId, session.user.id),
      isNull(member.revocationPendingAt),
    ),
  });
  if (
    !membership
    || membership.revocationPendingAt
    || !isWorkspaceRole(membership.role)
  ) {
    return { ok: false as const, status: 403, error: "Workspace access denied" };
  }
  if (!hasWorkspaceCapability(membership.role, capability)) {
    return { ok: false as const, status: 403, error: "Insufficient workspace permission" };
  }
  return {
    ok: true as const,
    session,
    membership,
    role: membership.role,
    accessMode: accessModeForRole(membership.role),
  };
}
