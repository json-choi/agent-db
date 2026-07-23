// Provider disconnection revokes live database credentials first, then the OAuth
// grant, and finally returns affected connections to member-local credential mode.
import { and, eq, isNull, sql } from "drizzle-orm";
import { db } from "../../../../../../../lib/db";
import { env } from "../../../../../../../lib/env";
import { isUuid, jsonError, mutationAllowed } from "../../../../../../../lib/http";
import {
  activeProviderIntegration,
  revokeActiveLeases,
  revokeProviderAuthorization,
} from "../../../../../../../lib/provider-integrations";
import { sealProviderCredential } from "../../../../../../../lib/secret-envelope";
import {
  workspaceAuditEvent,
  workspaceConnection,
  workspaceProviderIntegration,
} from "../../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../../lib/workspace-authorization";

type RouteContext = {
  params: Promise<{ workspaceId: string; integrationId: string }>;
};

export async function DELETE(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) {
    return jsonError("Invalid request origin", 403);
  }
  const { workspaceId, integrationId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(integrationId)) {
    return jsonError("Invalid workspace or integration id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const integration = await activeProviderIntegration(workspaceId, integrationId);
  if (!integration) return jsonError("Provider integration not found", 404);

  const revocation = await revokeActiveLeases({
    organizationId: workspaceId,
    integrationId,
  });
  if (revocation.deferred > 0) {
    return jsonError(
      "Active database access could not be revoked yet. Retry before disconnecting.",
      409,
    );
  }
  try {
    await revokeProviderAuthorization(integration);
  } catch {
    return jsonError("Provider authorization could not be revoked. Retry shortly.", 502);
  }
  const disconnectedAt = new Date();
  const scrubbedCredential = sealProviderCredential(integrationId, {
    revokedAt: disconnectedAt.toISOString(),
  });
  await db.batch([
    db.update(workspaceConnection).set({
      credentialMode: "member_local",
      providerIntegrationId: null,
      providerResource: null,
      revision: sql`${workspaceConnection.revision} + 1`,
      updatedAt: disconnectedAt,
    }).where(and(
      eq(workspaceConnection.organizationId, workspaceId),
      eq(workspaceConnection.providerIntegrationId, integrationId),
      isNull(workspaceConnection.deletedAt),
    )),
    db.update(workspaceProviderIntegration).set({
      status: "revoked",
      encryptedCredential: scrubbedCredential,
      credentialExpiresAt: null,
      grantedScope: null,
      revokedAt: disconnectedAt,
      updatedAt: disconnectedAt,
    }).where(and(
      eq(workspaceProviderIntegration.id, integrationId),
      eq(workspaceProviderIntegration.organizationId, workspaceId),
    )),
    db.insert(workspaceAuditEvent).values({
      organizationId: workspaceId,
      actorUserId: authorization.session.user.id,
      action: "provider.disconnect",
      resourceType: "provider_integration",
      resourceId: integrationId,
      redactedSummary: { provider: integration.provider, revokedLeases: revocation.revoked },
      requestId: crypto.randomUUID(),
    }),
  ]);
  return new Response(null, {
    status: 204,
    headers: { "cache-control": "private, no-store" },
  });
}
