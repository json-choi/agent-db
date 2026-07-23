// Redacted provider resource discovery. Every path identifier is validated inside
// its adapter and encrypted provider credentials never cross this boundary.
import {
  activeProviderIntegration,
  discoverProviderResources,
} from "../../../../../../../../lib/provider-integrations";
import { vercelOidcToken } from "../../../../../../../../lib/providers/gcp-cloud-sql";
import { ProviderRequestError } from "../../../../../../../../lib/providers/provider-types";
import { isUuid, jsonError, privateJson } from "../../../../../../../../lib/http";
import { authorizeWorkspace } from "../../../../../../../../lib/workspace-authorization";

type RouteContext = {
  params: Promise<{ workspaceId: string; integrationId: string }>;
};

export async function GET(request: Request, context: RouteContext) {
  const { workspaceId, integrationId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(integrationId)) {
    return jsonError("Invalid workspace or integration id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const integration = await activeProviderIntegration(workspaceId, integrationId);
  if (!integration) return jsonError("Provider integration not found", 404);
  const url = new URL(request.url);
  const kind = url.searchParams.get("kind") ?? "";
  const selection = Object.fromEntries(
    ["organization", "project", "database", "branch", "instance", "engine"]
      .map((key) => [key, url.searchParams.get(key) ?? ""]),
  );
  try {
    return privateJson({
      resources: await discoverProviderResources({
        integration,
        kind,
        selection,
        oidcToken: vercelOidcToken(request),
      }),
    });
  } catch (error) {
    if (error instanceof ProviderRequestError) {
      return jsonError(error.message, error.status);
    }
    return jsonError("Provider resource discovery failed", 502);
  }
}
