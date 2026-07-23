// Admin configuration for managed credentials. Resource existence is checked through
// the provider before the redacted selector is attached to a shared connection.
import { and, eq, exists, isNull, sql } from "drizzle-orm";
import { db } from "../../../../../../../../lib/db";
import { env } from "../../../../../../../../lib/env";
import {
  isUuid,
  jsonError,
  mutationAllowed,
  privateJson,
} from "../../../../../../../../lib/http";
import {
  activeProviderIntegration,
  parseManagedProviderResource,
  revokeActiveLeases,
  validateManagedProviderResource,
  type ManagedProviderResource,
} from "../../../../../../../../lib/provider-integrations";
import { vercelOidcToken } from "../../../../../../../../lib/providers/gcp-cloud-sql";
import { ProviderRequestError } from "../../../../../../../../lib/providers/provider-types";
import {
  claimRevocationGate,
  clearRevocationGate,
  releaseRevocationGateClaim,
  revocationGateLockKey,
} from "../../../../../../../../lib/revocation-gates";
import {
  workspaceAuditEvent,
  workspaceConnection,
  workspaceProviderIntegration,
} from "../../../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../../../lib/workspace-authorization";
import { publicConnection } from "../../../../../../../../lib/workspace-connections";

type RouteContext = {
  params: Promise<{ workspaceId: string; connectionId: string }>;
};

export async function PUT(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) {
    return jsonError("Invalid request origin", 403);
  }
  const { workspaceId, connectionId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const connection = await db.query.workspaceConnection.findFirst({
    where: and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ),
  });
  if (!connection) return jsonError("Connection not found", 404);
  const body = (await request.json().catch(() => null)) as {
    mode?: unknown;
    integrationId?: unknown;
    resource?: unknown;
  } | null;
  if (body?.mode !== "managed" && body?.mode !== "member_local") {
    return jsonError("Invalid credential mode", 400);
  }

  let integrationId: string | null = null;
  let providerResource: ManagedProviderResource | null = null;
  let managedProvider: string | null = null;
  if (body.mode === "managed") {
    if (typeof body.integrationId !== "string" || !isUuid(body.integrationId)) {
      return jsonError("Provider integration is required", 400);
    }
    integrationId = body.integrationId;
    const integration = await activeProviderIntegration(workspaceId, integrationId);
    if (!integration) {
      return jsonError("Provider integration not found", 404);
    }
    try {
      providerResource = parseManagedProviderResource(integration.provider, body.resource);
      if (providerResource.engine !== connection.engine) {
        return jsonError("Provider database engine does not match the connection", 409);
      }
      await validateManagedProviderResource({
        integration,
        resource: providerResource,
        oidcToken: vercelOidcToken(request),
      });
      managedProvider = integration.provider;
    } catch (error) {
      if (error instanceof ProviderRequestError) {
        return jsonError(error.message, error.status);
      }
      return jsonError(
        error instanceof Error ? error.message : "Invalid provider resource",
        400,
      );
    }
  }

  const claim = await claimRevocationGate({
    kind: "connection",
    organizationId: workspaceId,
    connectionId,
  });
  if (!claim) {
    return jsonError("Another connection access change is already in progress", 409);
  }
  const expectedClaimRevision = connection.revision + (claim.firstPending ? 1 : 0);
  if (claim.connectionRevision !== expectedClaimRevision) {
    await (
      claim.firstPending
        ? clearRevocationGate(claim)
        : releaseRevocationGateClaim(claim)
    ).catch(() => false);
    return jsonError(
      "Connection changed concurrently. Retry the access update.",
      409,
    );
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
    return jsonError(
      "Active database access could not be revoked yet. Retry before changing access.",
      409,
    );
  }
  const updatedAt = new Date();
  const lockTarget = integrationId
    ? {
        kind: "integration" as const,
        organizationId: workspaceId,
        integrationId,
      }
    : {
        kind: "connection" as const,
        organizationId: workspaceId,
        connectionId,
      };
  const integrationReady = integrationId && managedProvider
    ? exists(
        db.select({ id: workspaceProviderIntegration.id })
          .from(workspaceProviderIntegration)
          .where(and(
            eq(workspaceProviderIntegration.id, integrationId),
            eq(workspaceProviderIntegration.organizationId, workspaceId),
            eq(workspaceProviderIntegration.provider, managedProvider),
            eq(workspaceProviderIntegration.status, "active"),
            isNull(workspaceProviderIntegration.revokedAt),
            isNull(workspaceProviderIntegration.revocationPendingAt),
          )),
      )
    : sql`TRUE`;
  const [, updatedRows] = await db.batch([
    db.execute(sql`
      SELECT pg_advisory_xact_lock(
        hashtextextended(${revocationGateLockKey(lockTarget)}, 0)
      )
    `),
    db.update(workspaceConnection).set({
      credentialMode: body.mode,
      providerIntegrationId: integrationId,
      providerResource,
      ...(body.mode === "managed" && managedProvider
        ? { provider: managedProvider }
        : {}),
      revocationPendingAt: null,
      revocationClaimedAt: null,
      revocationClaimId: null,
      // Invalidate snapshots fetched after the revocation gate opened but before
      // this credential-mode/provider mutation commits.
      revision: sql`${workspaceConnection.revision} + 1`,
      updatedAt,
    }).where(and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      eq(workspaceConnection.revocationClaimId, claim.claimId),
      eq(workspaceConnection.revision, expectedClaimRevision),
      isNull(workspaceConnection.deletedAt),
      integrationReady,
    )).returning(),
    db.execute(sql`
      INSERT INTO ${workspaceAuditEvent}
        ("organization_id", "actor_user_id", "action", "resource_type",
         "resource_id", "redacted_summary", "request_id")
      SELECT connection."organization_id", ${authorization.session.user.id},
             'connection.credential_mode.update', 'connection',
             connection."id"::text,
             jsonb_build_object(
               'mode', ${body.mode},
               'provider', ${body.mode === "managed" ? managedProvider : null},
               'revokedLeases', ${revocation.revoked}
             ),
             ${crypto.randomUUID()}::uuid
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
    return jsonError(
      "Connection or provider access changed concurrently. Retry the update.",
      409,
    );
  }
  return privateJson({
    connection: publicConnection(updated, authorization.role, authorization.accessMode),
  });
}
