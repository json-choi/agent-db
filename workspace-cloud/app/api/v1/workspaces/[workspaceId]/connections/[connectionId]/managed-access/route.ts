// Admin configuration for managed credentials. Resource existence is checked through
// the provider before the redacted selector is attached to a shared connection.
import { and, eq, isNull, sql } from "drizzle-orm";
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
  parsePlanetScaleResource,
  providerAccessToken,
  revokeActiveLeases,
} from "../../../../../../../../lib/provider-integrations";
import {
  PlanetScaleRequestError,
  validatePlanetScaleResource,
} from "../../../../../../../../lib/providers/planetscale";
import {
  workspaceAuditEvent,
  workspaceConnection,
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
  let providerResource: ReturnType<typeof parsePlanetScaleResource> | null = null;
  if (body.mode === "managed") {
    if (typeof body.integrationId !== "string" || !isUuid(body.integrationId)) {
      return jsonError("Provider integration is required", 400);
    }
    integrationId = body.integrationId;
    const integration = await activeProviderIntegration(workspaceId, integrationId);
    if (!integration || integration.provider !== "planetScale") {
      return jsonError("Provider integration not found", 404);
    }
    try {
      providerResource = parsePlanetScaleResource(body.resource);
      if (providerResource.engine !== connection.engine) {
        return jsonError("Provider database engine does not match the connection", 409);
      }
      const accessToken = await providerAccessToken(integration);
      await validatePlanetScaleResource(accessToken, providerResource);
    } catch (error) {
      if (error instanceof PlanetScaleRequestError) {
        return jsonError(error.message, error.status);
      }
      return jsonError(
        error instanceof Error ? error.message : "Invalid provider resource",
        400,
      );
    }
  }

  const revocation = await revokeActiveLeases({
    organizationId: workspaceId,
    connectionId,
  });
  if (revocation.deferred > 0) {
    return jsonError(
      "Active database access could not be revoked yet. Retry before changing access.",
      409,
    );
  }
  const updatedAt = new Date();
  const [updatedRows] = await db.batch([
    db.update(workspaceConnection).set({
      credentialMode: body.mode,
      providerIntegrationId: integrationId,
      providerResource,
      ...(body.mode === "managed" ? { provider: "planetScale" } : {}),
      revision: sql`${workspaceConnection.revision} + 1`,
      updatedAt,
    }).where(and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    )).returning(),
    db.insert(workspaceAuditEvent).values({
      organizationId: workspaceId,
      actorUserId: authorization.session.user.id,
      action: "connection.credential_mode.update",
      resourceType: "connection",
      resourceId: connectionId,
      redactedSummary: {
        mode: body.mode,
        provider: body.mode === "managed" ? "planetScale" : null,
        revokedLeases: revocation.revoked,
      },
      requestId: crypto.randomUUID(),
    }),
  ]);
  const updated = updatedRows[0];
  return privateJson({
    connection: publicConnection(updated, authorization.role, authorization.accessMode),
  });
}
