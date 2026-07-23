// Workspace provider integration inventory and OAuth initiation. Secret material is
// omitted by explicit projection and OAuth state is single-use, hashed server data.
import { createHash, randomBytes } from "node:crypto";
import { and, eq, isNull, lt } from "drizzle-orm";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import {
  isUuid,
  jsonError,
  mutationAllowed,
  privateJson,
} from "../../../../../../lib/http";
import { providerCatalog } from "../../../../../../lib/provider-catalog";
import { parsePlanetScaleResource } from "../../../../../../lib/provider-integrations";
import {
  isPlanetScaleConfigured,
  planetScaleAuthorizationUrl,
  PlanetScaleRequestError,
} from "../../../../../../lib/providers/planetscale";
import {
  providerOauthState,
  workspaceConnection,
  workspaceProviderIntegration,
} from "../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../lib/workspace-authorization";

type RouteContext = { params: Promise<{ workspaceId: string }> };

export async function GET(request: Request, context: RouteContext) {
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const [integrations, managedRows] = await Promise.all([
    db.select({
      id: workspaceProviderIntegration.id,
      provider: workspaceProviderIntegration.provider,
      status: workspaceProviderIntegration.status,
      displayName: workspaceProviderIntegration.displayName,
      credentialExpiresAt: workspaceProviderIntegration.credentialExpiresAt,
      grantedScope: workspaceProviderIntegration.grantedScope,
      createdAt: workspaceProviderIntegration.createdAt,
      updatedAt: workspaceProviderIntegration.updatedAt,
    }).from(workspaceProviderIntegration).where(and(
      eq(workspaceProviderIntegration.organizationId, workspaceId),
      eq(workspaceProviderIntegration.status, "active"),
    )),
    db.select({
      connectionId: workspaceConnection.id,
      integrationId: workspaceConnection.providerIntegrationId,
      resource: workspaceConnection.providerResource,
    }).from(workspaceConnection).where(and(
      eq(workspaceConnection.organizationId, workspaceId),
      eq(workspaceConnection.credentialMode, "managed"),
      isNull(workspaceConnection.deletedAt),
    )),
  ]);
  const managedConnections = managedRows.flatMap((row) => {
    if (!row.integrationId) return [];
    try {
      return [{
        connectionId: row.connectionId,
        integrationId: row.integrationId,
        resource: parsePlanetScaleResource(row.resource),
      }];
    } catch {
      return [];
    }
  });
  return privateJson({
    providers: providerCatalog.map((provider) => ({
      ...provider,
      configured: provider.id === "planetScale"
        ? isPlanetScaleConfigured()
        : false,
    })),
    integrations,
    managedConnections,
  });
}

export async function POST(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) {
    return jsonError("Invalid request origin", 403);
  }
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const body = (await request.json().catch(() => null)) as { provider?: unknown } | null;
  if (body?.provider !== "planetScale") {
    return jsonError("Managed access for this provider is not available", 409);
  }

  const state = randomBytes(32).toString("base64url");
  const stateHash = createHash("sha256").update(state).digest("base64url");
  try {
    await db.delete(providerOauthState)
      .where(lt(providerOauthState.expiresAt, new Date()));
    await db.insert(providerOauthState).values({
      organizationId: workspaceId,
      userId: authorization.session.user.id,
      provider: "planetScale",
      stateHash,
      expiresAt: new Date(Date.now() + 10 * 60 * 1_000),
    });
    return privateJson({ authorizationUrl: planetScaleAuthorizationUrl(state) });
  } catch (error) {
    if (error instanceof PlanetScaleRequestError) {
      return jsonError(error.message, error.status);
    }
    throw error;
  }
}
