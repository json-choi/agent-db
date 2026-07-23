// Admin-only membership management. Better Auth remains the source of truth for
// invitation acceptance and role changes; this route adds strict role choices and audit.
import { and, desc, eq, sql } from "drizzle-orm";
import { auth } from "../../../../../../lib/auth";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import { isUuid, jsonError, mutationAllowed, privateJson } from "../../../../../../lib/http";
import { revokeActiveLeases } from "../../../../../../lib/provider-integrations";
import {
  claimRevocationGate,
  clearRevocationGate,
  releaseRevocationGateClaim,
  renewRevocationGateClaim,
} from "../../../../../../lib/revocation-gates";
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
  if (!isUuid(memberId) || !isAssignableRole(body?.role)) {
    return jsonError("Invalid member role update", 400);
  }
  const existing = await db.query.member.findFirst({
    where: and(eq(member.id, memberId), eq(member.organizationId, workspaceId)),
  });
  if (!existing) return jsonError("Member not found", 404);
  const claim = await claimRevocationGate({
    kind: "member",
    organizationId: workspaceId,
    memberId,
    userId: existing.userId,
  });
  if (!claim) {
    return jsonError("Another member access change is already in progress", 409);
  }
  if (claim.kind !== "member" || !claim.memberRole) {
    await (
      claim.firstPending
        ? clearRevocationGate(claim)
        : releaseRevocationGateClaim(claim)
    ).catch(() => false);
    return jsonError("Member access changed concurrently. Retry the update.", 409);
  }
  if (claim.memberRole === "owner") {
    await (
      claim.firstPending
        ? clearRevocationGate(claim)
        : releaseRevocationGateClaim(claim)
    ).catch(() => false);
    return jsonError("Owner role cannot be changed here", 403);
  }
  let revocation = { revoked: 0, deferred: 0 };
  try {
    if (claim.memberRole !== body.role || !claim.firstPending) {
      revocation = await revokeActiveLeases({
        organizationId: workspaceId,
        userId: claim.userId,
      });
    }
  } catch (error) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    throw error;
  }
  if (revocation.deferred > 0) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError(
      "Active database access could not be revoked; retry after its lease expires",
      409,
    );
  }
  const renewedClaim = await renewRevocationGateClaim(claim);
  if (!renewedClaim) {
    return jsonError("Member access changed concurrently. Retry the update.", 409);
  }
  if (
    renewedClaim.kind !== "member"
    || renewedClaim.memberRole !== claim.memberRole
  ) {
    await releaseRevocationGateClaim(renewedClaim).catch(() => false);
    return jsonError("Member access changed concurrently. Retry the update.", 409);
  }
  const result = await db.execute<{
    id: string;
    organizationId: string;
    userId: string;
    role: string;
    createdAt: Date | string;
  }>(sql`
    WITH updated_member AS (
      UPDATE ${member} AS target
      SET "role" = ${body.role},
          "revocation_pending_at" = NULL,
          "revocation_claimed_at" = NULL,
          "revocation_claim_id" = NULL
      WHERE target."id" = ${memberId}
        AND target."organization_id" = ${workspaceId}
        AND target."user_id" = ${renewedClaim.userId}
        AND target."role" = ${renewedClaim.memberRole}
        AND target."role" <> 'owner'
        AND target."revocation_claim_id" = ${renewedClaim.claimId}::uuid
      RETURNING target."id", target."organization_id", target."user_id",
                target."role", target."created_at"
    ),
    audit_event AS (
      INSERT INTO ${workspaceAuditEvent}
        ("organization_id", "actor_user_id", "action", "resource_type",
         "resource_id", "redacted_summary", "request_id")
      SELECT updated_member."organization_id",
             ${authorization.session.user.id}, 'member.role.update', 'member',
             updated_member."id",
             jsonb_build_object(
               'from', ${renewedClaim.memberRole},
               'to', updated_member."role",
               'revokedLeases', ${revocation.revoked},
               'deferredRevocations', ${revocation.deferred}
             ),
             ${crypto.randomUUID()}::uuid
      FROM updated_member
      RETURNING "resource_id"
    )
    SELECT "id" AS "id", "organization_id" AS "organizationId",
           "user_id" AS "userId", "role" AS "role",
           "created_at" AS "createdAt"
    FROM updated_member
  `).catch(async (error) => {
    await releaseRevocationGateClaim(renewedClaim).catch(() => false);
    throw error;
  });
  const updated = result.rows[0];
  if (!updated) {
    await releaseRevocationGateClaim(renewedClaim).catch(() => false);
    return jsonError("Member access changed concurrently. Retry the update.", 409);
  }
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

  if (typeof body?.invitationId === "string" && isUuid(body.invitationId)) {
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

  if (typeof body?.memberId === "string" && isUuid(body.memberId)) {
    const existing = await db.query.member.findFirst({
      where: and(eq(member.id, body.memberId), eq(member.organizationId, workspaceId)),
    });
    if (!existing) return jsonError("Member not found", 404);
    const claim = await claimRevocationGate({
      kind: "member",
      organizationId: workspaceId,
      memberId: existing.id,
      userId: existing.userId,
    });
    if (!claim) {
      return jsonError("Another member access change is already in progress", 409);
    }
    if (claim.kind !== "member" || !claim.memberRole) {
      await (
        claim.firstPending
          ? clearRevocationGate(claim)
          : releaseRevocationGateClaim(claim)
      ).catch(() => false);
      return jsonError("Member access changed concurrently. Retry removal.", 409);
    }
    if (claim.memberRole === "owner") {
      await (
        claim.firstPending
          ? clearRevocationGate(claim)
          : releaseRevocationGateClaim(claim)
      ).catch(() => false);
      return jsonError("Owner cannot be removed", 403);
    }
    let revocation;
    try {
      revocation = await revokeActiveLeases({
        organizationId: workspaceId,
        userId: claim.userId,
      });
    } catch (error) {
      await releaseRevocationGateClaim(claim).catch(() => false);
      throw error;
    }
    if (revocation.deferred > 0) {
      await releaseRevocationGateClaim(claim).catch(() => false);
      return jsonError(
        "Active database access could not be revoked; retry after its lease expires",
        409,
      );
    }
    const renewedClaim = await renewRevocationGateClaim(claim);
    if (!renewedClaim) {
      return jsonError("Member access changed concurrently. Retry removal.", 409);
    }
    if (
      renewedClaim.kind !== "member"
      || renewedClaim.memberRole !== claim.memberRole
    ) {
      await releaseRevocationGateClaim(renewedClaim).catch(() => false);
      return jsonError("Member access changed concurrently. Retry removal.", 409);
    }
    const result = await db.execute<{ id: string }>(sql`
      WITH deleted_member AS (
        DELETE FROM ${member} AS target
        WHERE target."id" = ${existing.id}
          AND target."organization_id" = ${workspaceId}
          AND target."user_id" = ${renewedClaim.userId}
          AND target."role" = ${renewedClaim.memberRole}
          AND target."role" <> 'owner'
          AND target."revocation_claim_id" = ${renewedClaim.claimId}::uuid
        RETURNING target."id", target."organization_id", target."role"
      ),
      audit_event AS (
        INSERT INTO ${workspaceAuditEvent}
          ("organization_id", "actor_user_id", "action", "resource_type",
           "resource_id", "redacted_summary", "request_id")
        SELECT deleted_member."organization_id",
               ${authorization.session.user.id}, 'member.remove', 'member',
               deleted_member."id",
               jsonb_build_object(
                 'previousRole', deleted_member."role",
                 'revokedLeases', ${revocation.revoked},
                 'deferredRevocations', ${revocation.deferred}
               ),
               ${crypto.randomUUID()}::uuid
        FROM deleted_member
        RETURNING "resource_id"
      )
      SELECT "id" FROM deleted_member
    `).catch(async (error) => {
      await releaseRevocationGateClaim(renewedClaim).catch(() => false);
      throw error;
    });
    if (result.rows.length !== 1) {
      await releaseRevocationGateClaim(renewedClaim).catch(() => false);
      return jsonError("Member access changed concurrently. Retry removal.", 409);
    }
    return privateJson({ status: true });
  }

  return jsonError("Member or invitation id is required", 400);
}
