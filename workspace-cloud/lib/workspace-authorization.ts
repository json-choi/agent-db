// Server-side workspace authorization. Every resource request resolves the session
// and membership from the database and fails closed; client role claims are ignored.
import "server-only";

import { and, eq } from "drizzle-orm";
import { db } from "./db";
import { auth } from "./auth";
import { member } from "./schema";

export const workspaceRoleNames = ["viewer", "analyst", "editor", "admin", "owner"] as const;
export type WorkspaceRoleName = (typeof workspaceRoleNames)[number];
export type WorkspaceCapability = "view" | "read" | "write" | "manage" | "delete";

const roleRank: Record<WorkspaceRoleName, number> = {
  viewer: 0,
  analyst: 1,
  editor: 2,
  admin: 3,
  owner: 4,
};

const requiredRank: Record<WorkspaceCapability, number> = {
  view: roleRank.viewer,
  read: roleRank.analyst,
  write: roleRank.editor,
  manage: roleRank.admin,
  delete: roleRank.owner,
};

export function isWorkspaceRole(value: string): value is WorkspaceRoleName {
  return workspaceRoleNames.includes(value as WorkspaceRoleName);
}

export function accessModeForRole(role: WorkspaceRoleName) {
  if (roleRank[role] >= roleRank.admin) return "manage" as const;
  if (roleRank[role] >= roleRank.editor) return "write" as const;
  if (roleRank[role] >= roleRank.analyst) return "read" as const;
  return "view" as const;
}

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
    ),
  });
  if (!membership || !isWorkspaceRole(membership.role)) {
    return { ok: false as const, status: 403, error: "Workspace access denied" };
  }
  if (roleRank[membership.role] < requiredRank[capability]) {
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
