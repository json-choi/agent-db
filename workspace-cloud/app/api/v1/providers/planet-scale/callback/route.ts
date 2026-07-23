// PlanetScale OAuth callback. State is consumed before code exchange and bound to the
// current Better Auth user, preventing replay and cross-account integration swapping.
import { createHash } from "node:crypto";
import { and, eq, gt } from "drizzle-orm";
import { auth } from "../../../../../../lib/auth";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import {
  exchangePlanetScaleCode,
  inspectPlanetScaleToken,
  revokePlanetScaleAuthorization,
} from "../../../../../../lib/providers/planetscale";
import { missingPlanetScaleManagedScopes } from "../../../../../../lib/providers/planetscale-core";
import { revokeProviderAuthorization } from "../../../../../../lib/provider-integrations";
import { sealProviderCredential } from "../../../../../../lib/secret-envelope";
import {
  providerOauthState,
  workspaceAuditEvent,
  workspaceProviderIntegration,
} from "../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../lib/workspace-authorization";

function settingsUrl(
  workspaceId: string | null,
  status: "connected" | "failed",
) {
  const target = new URL("/settings", env.appOrigin());
  target.searchParams.set("provider", "planetScale");
  target.searchParams.set("status", status);
  if (workspaceId) {
    target.searchParams.set("workspace", workspaceId);
    target.hash = `workspace-${workspaceId}`;
  }
  return target;
}

export async function GET(request: Request) {
  const url = new URL(request.url);
  const state = url.searchParams.get("state") ?? "";
  const code = url.searchParams.get("code") ?? "";
  if (
    state.length < 32
    || state.length > 256
    || code.length < 8
    || code.length > 2_048
  ) {
    return Response.redirect(settingsUrl(null, "failed"));
  }
  const session = await auth.api.getSession({ headers: request.headers });
  if (!session) {
    return Response.redirect(new URL(
      `/auth/sign-in?returnTo=${encodeURIComponent("/settings")}`,
      env.appOrigin(),
    ));
  }
  const stateHash = createHash("sha256").update(state).digest("base64url");
  const consumed = await db.delete(providerOauthState).where(and(
    eq(providerOauthState.stateHash, stateHash),
    eq(providerOauthState.userId, session.user.id),
    eq(providerOauthState.provider, "planetScale"),
    gt(providerOauthState.expiresAt, new Date()),
  )).returning({
    organizationId: providerOauthState.organizationId,
  });
  const oauthState = consumed[0];
  if (!oauthState) return Response.redirect(settingsUrl(null, "failed"));
  const authorization = await authorizeWorkspace(
    request,
    oauthState.organizationId,
    "manage",
  );
  if (!authorization.ok || authorization.session.user.id !== session.user.id) {
    return Response.redirect(settingsUrl(oauthState.organizationId, "failed"));
  }

  let refreshTokenToRevoke = "";
  try {
    const token = await exchangePlanetScaleCode(code);
    refreshTokenToRevoke = token.refreshToken;
    const tokenInfo = await inspectPlanetScaleToken(token.accessToken);
    const verifiedScope = tokenInfo.scope || token.scope;
    if (missingPlanetScaleManagedScopes(verifiedScope).length > 0) {
      throw new Error("PlanetScale authorization omitted required managed-access scopes");
    }
    const existing = await db.query.workspaceProviderIntegration.findFirst({
      where: and(
        eq(workspaceProviderIntegration.organizationId, oauthState.organizationId),
        eq(workspaceProviderIntegration.provider, "planetScale"),
        eq(workspaceProviderIntegration.externalAccountId, tokenInfo.subject),
      ),
      columns: {
        id: true,
        organizationId: true,
        provider: true,
        encryptedCredential: true,
        credentialExpiresAt: true,
      },
    });
    const integrationId = existing?.id ?? crypto.randomUUID();
    const encryptedCredential = sealProviderCredential(integrationId, {
      ...token,
      scope: verifiedScope,
    });
    if (existing) {
      await db.batch([
        db.update(workspaceProviderIntegration).set({
          status: "active",
          displayName: `PlanetScale · ${tokenInfo.subject.slice(-8)}`,
          encryptedCredential,
          credentialExpiresAt: new Date(token.expiresAt),
          grantedScope: verifiedScope,
          updatedAt: new Date(),
          revokedAt: null,
        }).where(eq(workspaceProviderIntegration.id, integrationId)),
        db.insert(workspaceAuditEvent).values({
          organizationId: oauthState.organizationId,
          actorUserId: session.user.id,
          action: "provider.connect",
          resourceType: "provider_integration",
          resourceId: integrationId,
          redactedSummary: { provider: "planetScale" },
          requestId: crypto.randomUUID(),
        }),
      ]);
      await revokeProviderAuthorization(existing).catch(() => undefined);
    } else {
      await db.batch([
        db.insert(workspaceProviderIntegration).values({
          id: integrationId,
          organizationId: oauthState.organizationId,
          provider: "planetScale",
          externalAccountId: tokenInfo.subject,
          displayName: `PlanetScale · ${tokenInfo.subject.slice(-8)}`,
          encryptedCredential,
          credentialExpiresAt: new Date(token.expiresAt),
          grantedScope: verifiedScope,
          createdByUserId: session.user.id,
        }),
        db.insert(workspaceAuditEvent).values({
          organizationId: oauthState.organizationId,
          actorUserId: session.user.id,
          action: "provider.connect",
          resourceType: "provider_integration",
          resourceId: integrationId,
          redactedSummary: { provider: "planetScale" },
          requestId: crypto.randomUUID(),
        }),
      ]);
    }
    return Response.redirect(settingsUrl(oauthState.organizationId, "connected"));
  } catch {
    if (refreshTokenToRevoke) {
      await revokePlanetScaleAuthorization(refreshTokenToRevoke).catch(() => undefined);
    }
    return Response.redirect(settingsUrl(oauthState.organizationId, "failed"));
  }
}
