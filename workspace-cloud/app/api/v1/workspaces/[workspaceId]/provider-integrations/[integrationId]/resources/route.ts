// Redacted provider resource discovery. Every path identifier comes from strict
// allowlisted parsing and the encrypted OAuth token never crosses this boundary.
import { activeProviderIntegration, providerAccessToken } from "../../../../../../../../lib/provider-integrations";
import {
  listPlanetScaleBranches,
  listPlanetScaleDatabases,
  listPlanetScaleOrganizations,
  PlanetScaleRequestError,
} from "../../../../../../../../lib/providers/planetscale";
import { isUuid, jsonError, privateJson } from "../../../../../../../../lib/http";
import { authorizeWorkspace } from "../../../../../../../../lib/workspace-authorization";

type RouteContext = {
  params: Promise<{ workspaceId: string; integrationId: string }>;
};

function resourceSegment(value: string | null) {
  return value && /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(value)
    ? value
    : null;
}

export async function GET(request: Request, context: RouteContext) {
  const { workspaceId, integrationId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(integrationId)) {
    return jsonError("Invalid workspace or integration id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const integration = await activeProviderIntegration(workspaceId, integrationId);
  if (!integration) return jsonError("Provider integration not found", 404);
  if (integration.provider !== "planetScale") {
    return jsonError("Provider resource discovery is not available", 409);
  }

  const url = new URL(request.url);
  const kind = url.searchParams.get("kind");
  const organization = resourceSegment(url.searchParams.get("organization"));
  const database = resourceSegment(url.searchParams.get("database"));
  try {
    const accessToken = await providerAccessToken(integration);
    if (kind === "organizations") {
      return privateJson({ resources: await listPlanetScaleOrganizations(accessToken) });
    }
    if (kind === "databases" && organization) {
      return privateJson({
        resources: await listPlanetScaleDatabases(accessToken, organization),
      });
    }
    if (kind === "branches" && organization && database) {
      return privateJson({
        resources: await listPlanetScaleBranches(accessToken, organization, database),
      });
    }
    return jsonError("Invalid provider resource query", 400);
  } catch (error) {
    if (error instanceof PlanetScaleRequestError) {
      return jsonError(error.message, error.status);
    }
    return jsonError("Provider resource discovery failed", 502);
  }
}
