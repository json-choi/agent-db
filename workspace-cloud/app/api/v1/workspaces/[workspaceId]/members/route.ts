// Admin-only membership management. Better Auth remains the source of truth for
// invitation acceptance and role changes; this route adds strict role choices and audit.
import { and, desc, eq } from "drizzle-orm";
import { auth } from "../../../../../../lib/auth";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import { isUuid, jsonError, mutationAllowed, privateJson } from "../../../../../../lib/http";
import { invitation, member, user, workspaceAuditEvent } from "../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../lib/workspace-authorization";

type RouteContext = { params: Promise<{ workspaceId: string }> };
const assignableRoles = ["viewer", "analyst", "editor", "admin"] as const;
type AssignableRole = (typeof assignableRoles)[number];

function isAssignableRole(value: unknown): value is AssignableRole {
  return typeof value === "string" && assignableRoles.includes(value as AssignableRole);
}

export async function GET(request: Request, context: RouteContext) {
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const [members, invitations] = await Promise.all([
    db.select({
      id: member.id,
      userId: member.userId,
      name: user.name,
      email: user.email,
      role: member.role,
      createdAt: member.createdAt,
    }).from(member).innerJoin(user, eq(member.userId, user.id))
      .where(eq(member.organizationId, workspaceId)).orderBy(desc(member.createdAt)),
    db.select({
      id: invitation.id,
      email: invitation.email,
      role: invitation.role,
      status: invitation.status,
      expiresAt: invitation.expiresAt,
      createdAt: invitation.createdAt,
    }).from(invitation).where(and(
      eq(invitation.organizationId, workspaceId),
      eq(invitation.status, "pending"),
    )).orderBy(desc(invitation.createdAt)),
  ]);
  return privateJson({
    workspaceId,
    members,
    invitations: invitations.map((item) => ({
      ...item,
      inviteUrl: `${env.appOrigin()}/accept-invitation/${encodeURIComponent(item.id)}`,
    })),
  });
}

export async function POST(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const body = (await request.json().catch(() => null)) as { email?: unknown; role?: unknown } | null;
  const email = typeof body?.email === "string" ? body.email.trim().toLowerCase() : "";
  if (!/^\S+@\S+\.\S+$/.test(email) || email.length > 320) return jsonError("Invalid email", 400);
  if (!isAssignableRole(body?.role)) return jsonError("Invalid assignable workspace role", 400);

  const created = await auth.api.createInvitation({
    headers: request.headers,
    body: { email, role: body.role, organizationId: workspaceId, resend: true },
  });
  await db.insert(workspaceAuditEvent).values({
    organizationId: workspaceId,
    actorUserId: authorization.session.user.id,
    action: "member.invite",
    resourceType: "invitation",
    resourceId: created.id,
    redactedSummary: { role: body.role, emailDomain: email.split("@")[1] },
    requestId: crypto.randomUUID(),
  });
  return privateJson({
    invitation: {
      ...created,
      inviteUrl: `${env.appOrigin()}/accept-invitation/${encodeURIComponent(created.id)}`,
    },
  }, { status: 201 });
}

export async function PATCH(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const body = (await request.json().catch(() => null)) as { memberId?: unknown; role?: unknown } | null;
  const memberId = typeof body?.memberId === "string" ? body.memberId : "";
  if (!memberId || !isAssignableRole(body?.role)) return jsonError("Invalid member role update", 400);
  const existing = await db.query.member.findFirst({
    where: and(eq(member.id, memberId), eq(member.organizationId, workspaceId)),
  });
  if (!existing) return jsonError("Member not found", 404);
  if (existing.role === "owner") return jsonError("Owner role cannot be changed here", 403);
  const updated = await auth.api.updateMemberRole({
    headers: request.headers,
    body: { memberId, role: body.role, organizationId: workspaceId },
  });
  await db.insert(workspaceAuditEvent).values({
    organizationId: workspaceId,
    actorUserId: authorization.session.user.id,
    action: "member.role.update",
    resourceType: "member",
    resourceId: memberId,
    redactedSummary: { from: existing.role, to: body.role },
    requestId: crypto.randomUUID(),
  });
  return privateJson({ member: updated });
}

export async function DELETE(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const body = (await request.json().catch(() => null)) as {
    memberId?: unknown;
    invitationId?: unknown;
  } | null;

  if (typeof body?.invitationId === "string" && body.invitationId) {
    const existing = await db.query.invitation.findFirst({
      where: and(
        eq(invitation.id, body.invitationId),
        eq(invitation.organizationId, workspaceId),
        eq(invitation.status, "pending"),
      ),
    });
    if (!existing) return jsonError("Invitation not found", 404);
    await auth.api.cancelInvitation({
      headers: request.headers,
      body: { invitationId: existing.id },
    });
    await db.insert(workspaceAuditEvent).values({
      organizationId: workspaceId,
      actorUserId: authorization.session.user.id,
      action: "member.invite.cancel",
      resourceType: "invitation",
      resourceId: existing.id,
      redactedSummary: { emailDomain: existing.email.split("@")[1] },
      requestId: crypto.randomUUID(),
    });
    return privateJson({ status: true });
  }

  if (typeof body?.memberId === "string" && body.memberId) {
    const existing = await db.query.member.findFirst({
      where: and(eq(member.id, body.memberId), eq(member.organizationId, workspaceId)),
    });
    if (!existing) return jsonError("Member not found", 404);
    if (existing.role === "owner") return jsonError("Owner cannot be removed", 403);
    await auth.api.removeMember({
      headers: request.headers,
      body: { memberIdOrEmail: existing.id, organizationId: workspaceId },
    });
    await db.insert(workspaceAuditEvent).values({
      organizationId: workspaceId,
      actorUserId: authorization.session.user.id,
      action: "member.remove",
      resourceType: "member",
      resourceId: existing.id,
      redactedSummary: { previousRole: existing.role },
      requestId: crypto.randomUUID(),
    });
    return privateJson({ status: true });
  }

  return jsonError("Member or invitation id is required", 400);
}
