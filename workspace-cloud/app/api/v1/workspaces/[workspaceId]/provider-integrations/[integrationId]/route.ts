// Provider disconnection revokes live database credentials first, then the OAuth
// grant, and finally returns affected connections to member-local credential mode.
import { sql } from "drizzle-orm";
import { db } from "../../../../../../../lib/db";
import { env } from "../../../../../../../lib/env";
import { isUuid, jsonError, mutationAllowed } from "../../../../../../../lib/http";
import {
  providerIntegrationForRevocation,
  revokeActiveLeases,
  revokeProviderAuthorization,
} from "../../../../../../../lib/provider-integrations";
import {
  claimRevocationGate,
  releaseRevocationGateClaim,
} from "../../../../../../../lib/revocation-gates";
import { sealProviderCredential } from "../../../../../../../lib/secret-envelope";
import {
  workspaceAuditEvent,
  workspaceConnection,
  workspaceProviderIntegration,
  workspaceProviderPrincipalClaim,
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
  const claim = await claimRevocationGate({
    kind: "integration",
    organizationId: workspaceId,
    integrationId,
  });
  if (!claim) {
    const existing = await providerIntegrationForRevocation(workspaceId, integrationId);
    return existing
      ? jsonError("Another provider access change is already in progress", 409)
      : jsonError("Provider integration not found", 404);
  }
  const integration = await providerIntegrationForRevocation(workspaceId, integrationId);
  if (!integration) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError("Provider integration not found", 404);
  }

  let revocation;
  try {
    revocation = await revokeActiveLeases({
      organizationId: workspaceId,
      integrationId,
    });
  } catch (error) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    throw error;
  }
  if (revocation.deferred > 0) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError(
      "Active database access could not be revoked yet. Retry before disconnecting.",
      409,
    );
  }
  try {
    await revokeProviderAuthorization(integration);
  } catch {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError("Provider authorization could not be revoked. Retry shortly.", 502);
  }
  const disconnectedAt = new Date();
  const scrubbedCredential = sealProviderCredential(integrationId, {
    revokedAt: disconnectedAt.toISOString(),
  });
  const result = await db.execute<{ id: string }>(sql`
    WITH revoked_integration AS (
      UPDATE ${workspaceProviderIntegration} AS integration
      SET "status" = 'revoked',
          "encrypted_credential" = ${scrubbedCredential},
          "credential_expires_at" = NULL,
          "granted_scope" = NULL,
          "revoked_at" = ${disconnectedAt},
          "updated_at" = ${disconnectedAt},
          "revocation_pending_at" = NULL,
          "revocation_claimed_at" = NULL,
          "revocation_claim_id" = NULL
      WHERE integration."id" = ${integrationId}::uuid
        AND integration."organization_id" = ${workspaceId}
        AND integration."status" = 'active'
        AND integration."revoked_at" IS NULL
        AND integration."revocation_claim_id" = ${claim.claimId}::uuid
      RETURNING integration."id", integration."organization_id"
    ),
    detached_connections AS (
      UPDATE ${workspaceConnection} AS connection
      SET "credential_mode" = 'member_local',
          "provider_integration_id" = NULL,
          "provider_resource" = NULL,
          "revision" = connection."revision" + 1,
          "updated_at" = ${disconnectedAt}
      FROM revoked_integration
      WHERE connection."organization_id" = revoked_integration."organization_id"
        AND connection."provider_integration_id" = revoked_integration."id"
        AND connection."deleted_at" IS NULL
      RETURNING connection."id"
    ),
    deleted_principal_claims AS (
      DELETE FROM ${workspaceProviderPrincipalClaim} AS claim
      USING revoked_integration
      WHERE claim."integration_id" = revoked_integration."id"
      RETURNING claim."principal_fingerprint"
    ),
    audit_event AS (
      INSERT INTO ${workspaceAuditEvent}
        ("organization_id", "actor_user_id", "action", "resource_type",
         "resource_id", "redacted_summary", "request_id")
      SELECT revoked_integration."organization_id",
             ${authorization.session.user.id}, 'provider.disconnect',
             'provider_integration', revoked_integration."id"::text,
             jsonb_build_object(
               'provider', ${integration.provider},
               'revokedLeases', ${revocation.revoked}
             ),
             ${crypto.randomUUID()}::uuid
      FROM revoked_integration
      RETURNING "resource_id"
    )
    SELECT "id"::text AS "id" FROM revoked_integration
  `).catch(async (error) => {
    await releaseRevocationGateClaim(claim).catch(() => false);
    throw error;
  });
  if (result.rows.length !== 1) {
    await releaseRevocationGateClaim(claim).catch(() => false);
    return jsonError("Provider access changed concurrently. Retry disconnecting.", 409);
  }
  return new Response(null, {
    status: 204,
    headers: { "cache-control": "private, no-store" },
  });
}
