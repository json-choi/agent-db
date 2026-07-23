// Mutation surface for one shared template. UUID lookup is always intersected with
// the authenticated organization to prevent cross-workspace identifier access.
import { and, eq, isNull, sql } from "drizzle-orm";
import { db } from "../../../../../../../lib/db";
import { env } from "../../../../../../../lib/env";
import { isUuid, jsonError, mutationAllowed, privateJson } from "../../../../../../../lib/http";
import { revokeActiveLeases } from "../../../../../../../lib/provider-integrations";
import {
  claimRevocationGate,
  clearRevocationGate,
  releaseRevocationGateClaim,
} from "../../../../../../../lib/revocation-gates";
import {
  workspaceAuditEvent,
  workspaceConnection,
  workspaceProviderIntegration,
} from "../../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../../lib/workspace-authorization";
import { parseSharedConnection, publicConnection } from "../../../../../../../lib/workspace-connections";

type RouteContext = { params: Promise<{ workspaceId: string; connectionId: string }> };

export async function POST(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
  const body = (await request.json().catch(() => null)) as { action?: unknown } | null;
  if (body?.action !== "read" && body?.action !== "write") {
    return jsonError("Action must be read or write", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, body.action);
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const [connection] = await db.select({
    id: workspaceConnection.id,
    revision: workspaceConnection.revision,
    allowWrites: workspaceConnection.allowWrites,
    credentialMode: workspaceConnection.credentialMode,
    provider: workspaceConnection.provider,
    providerIntegrationId: workspaceConnection.providerIntegrationId,
    revocationPendingAt: workspaceConnection.revocationPendingAt,
    integrationStatus: workspaceProviderIntegration.status,
    integrationProvider: workspaceProviderIntegration.provider,
    integrationRevokedAt: workspaceProviderIntegration.revokedAt,
    integrationRevocationPendingAt:
      workspaceProviderIntegration.revocationPendingAt,
    integrationRevocationClaimId:
      workspaceProviderIntegration.revocationClaimId,
  }).from(workspaceConnection).leftJoin(
    workspaceProviderIntegration,
    and(
      eq(
        workspaceProviderIntegration.id,
        workspaceConnection.providerIntegrationId,
      ),
      eq(
        workspaceProviderIntegration.organizationId,
        workspaceConnection.organizationId,
      ),
    ),
  ).where(and(
    eq(workspaceConnection.id, connectionId),
    eq(workspaceConnection.organizationId, workspaceId),
    isNull(workspaceConnection.deletedAt),
  )).limit(1);
  if (!connection) return jsonError("Connection not found", 404);
  if (connection.revocationPendingAt) {
    return jsonError("Connection access is changing. Retry shortly.", 409);
  }
  if (
    connection.credentialMode === "managed"
    && (
      !connection.providerIntegrationId
      || connection.integrationProvider !== connection.provider
      || connection.integrationStatus !== "active"
      || connection.integrationRevokedAt !== null
      || connection.integrationRevocationPendingAt !== null
      || connection.integrationRevocationClaimId !== null
    )
  ) {
    return jsonError("Managed provider access is unavailable or changing", 409);
  }
  if (body.action === "write" && !connection.allowWrites) {
    return jsonError("Writing is disabled for this connection", 403);
  }
  return privateJson({
    allowed: true,
    action: body.action,
    role: authorization.role,
    accessMode: authorization.accessMode,
    revision: connection.revision,
  });
}

export async function PATCH(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const existing = await db.query.workspaceConnection.findFirst({
    where: and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ),
    columns: {
      id: true,
      engine: true,
      provider: true,
      credentialMode: true,
      revision: true,
    },
  });
  if (!existing) return jsonError("Connection not found", 404);
  let input;
  try {
    input = parseSharedConnection(await request.json());
  } catch (error) {
    return jsonError(error instanceof Error ? error.message : "Invalid connection template", 400);
  }
  if (existing.credentialMode === "managed" && input.engine !== existing.engine) {
    return jsonError(
      "Switch to member-local credentials before changing a managed database engine",
      409,
    );
  }
  const claim = await claimRevocationGate({
    kind: "connection",
    organizationId: workspaceId,
    connectionId,
  });
  if (!claim) {
    return jsonError("Another connection access change is already in progress", 409);
  }
  const expectedClaimRevision = existing.revision + (claim.firstPending ? 1 : 0);
  if (claim.connectionRevision !== expectedClaimRevision) {
    await (
      claim.firstPending
        ? clearRevocationGate(claim)
        : releaseRevocationGateClaim(claim)
    ).catch(() => false);
    return jsonError("Connection changed concurrently. Retry the update.", 409);
  }
  let revocation;
  try {
    revocation = await revokeActiveLeases({
      organizationId: workspaceId,
      connectionId,
    });
  } catch (error) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    throw error;
  }
  if (revocation.deferred > 0) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError("Active database access could not be revoked. Retry the update.", 409);
  }
  const updatedAt = new Date();
  const requestId = crypto.randomUUID();
  const [updatedRows] = await db.batch([
    db.update(workspaceConnection)
      .set({
        name: input.name,
        engine: input.engine,
        provider: existing.credentialMode === "managed" ? existing.provider : input.provider,
        driverId: input.driverId,
        host: input.host,
        port: input.port,
        databaseName: input.database,
        sslmode: input.sslmode,
        readonlyDefault: input.readonlyDefault,
        allowWrites: input.allowWrites,
        environment: input.env,
        schemaGroup: input.schemaGroup,
        revocationPendingAt: null,
        revocationClaimedAt: null,
        revocationClaimId: null,
        // The gate revision invalidates operations that started before revocation.
        // This second bump invalidates any template snapshot fetched while the gate
        // was pending, before the material update committed.
        revision: sql`${workspaceConnection.revision} + 1`,
        updatedAt,
      })
      .where(and(
        eq(workspaceConnection.id, connectionId),
        eq(workspaceConnection.organizationId, workspaceId),
        eq(workspaceConnection.revocationClaimId, claim.claimId),
        eq(workspaceConnection.revision, expectedClaimRevision),
        isNull(workspaceConnection.deletedAt),
      ))
      .returning(),
    db.execute(sql`
      INSERT INTO ${workspaceAuditEvent}
        ("organization_id", "actor_user_id", "action", "resource_type",
         "resource_id", "redacted_summary", "request_id")
      SELECT connection."organization_id", ${authorization.session.user.id},
             'connection.update', 'connection', connection."id"::text,
             jsonb_build_object('name', connection."name", 'revision', connection."revision"),
             ${requestId}::uuid
      FROM ${workspaceConnection} AS connection
      WHERE connection."id" = ${connectionId}::uuid
        AND connection."organization_id" = ${workspaceId}
        AND connection."updated_at" = ${updatedAt}
        AND connection."deleted_at" IS NULL
    `),
  ]).catch(async (error) => {
    await releaseRevocationGateClaim(claim).catch(() => false);
    throw error;
  });
  const updated = updatedRows[0];
  if (!updated) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError("Connection access changed concurrently. Retry the update.", 409);
  }
  return privateJson({
    connection: publicConnection(updated, authorization.role, authorization.accessMode),
  });
}

export async function DELETE(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const existing = await db.query.workspaceConnection.findFirst({
    where: and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ),
    columns: { id: true },
  });
  if (!existing) return jsonError("Connection not found", 404);
  const claim = await claimRevocationGate({
    kind: "connection",
    organizationId: workspaceId,
    connectionId,
  });
  if (!claim) {
    return jsonError("Another connection access change is already in progress", 409);
  }
  let revocation;
  try {
    revocation = await revokeActiveLeases({
      organizationId: workspaceId,
      connectionId,
    });
  } catch (error) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    throw error;
  }
  if (revocation.deferred > 0) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError("Active database access could not be revoked. Retry deletion.", 409);
  }
  const deletedAt = new Date();
  const requestId = crypto.randomUUID();
  const [deletedRows] = await db.batch([
    db.update(workspaceConnection)
      .set({
        deletedAt,
        updatedAt: deletedAt,
        revocationPendingAt: null,
        revocationClaimedAt: null,
        revocationClaimId: null,
      })
      .where(and(
        eq(workspaceConnection.id, connectionId),
        eq(workspaceConnection.organizationId, workspaceId),
        eq(workspaceConnection.revocationClaimId, claim.claimId),
        isNull(workspaceConnection.deletedAt),
      ))
      .returning({ id: workspaceConnection.id, name: workspaceConnection.name }),
    db.execute(sql`
      INSERT INTO ${workspaceAuditEvent}
        ("organization_id", "actor_user_id", "action", "resource_type",
         "resource_id", "redacted_summary", "request_id")
      SELECT connection."organization_id", ${authorization.session.user.id},
             'connection.delete', 'connection', connection."id"::text,
             jsonb_build_object('name', connection."name"), ${requestId}::uuid
      FROM ${workspaceConnection} AS connection
      WHERE connection."id" = ${connectionId}::uuid
        AND connection."organization_id" = ${workspaceId}
        AND connection."deleted_at" = ${deletedAt}
    `),
  ]).catch(async (error) => {
    await releaseRevocationGateClaim(claim).catch(() => false);
    throw error;
  });
  const deleted = deletedRows[0];
  if (!deleted) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError("Connection access changed concurrently. Retry deletion.", 409);
  }
  return new Response(null, {
    status: 204,
    headers: { "cache-control": "private, no-store" },
  });
}
