// PlanetScale OAuth callback. State is consumed before code exchange and bound to the
// current Better Auth user, preventing replay and cross-account integration swapping.
import { createHash } from "node:crypto";
import { and, eq, gt, isNull, sql } from "drizzle-orm";
import { auth } from "../../../../../../lib/auth";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import {
  exchangePlanetScaleCode,
  inspectPlanetScaleToken,
  revokePlanetScaleAuthorization,
} from "../../../../../../lib/providers/planetscale";
import { missingPlanetScaleManagedScopes } from "../../../../../../lib/providers/planetscale-core";
import {
  revokeActiveLeases,
  revokeProviderAuthorization,
} from "../../../../../../lib/provider-integrations";
import {
  claimRevocationGate,
  releaseRevocationGateClaim,
  type RevocationGateClaim,
} from "../../../../../../lib/revocation-gates";
import { sealProviderCredential } from "../../../../../../lib/secret-envelope";
import {
  providerOauthState,
  workspaceAuditEvent,
  workspaceConnection,
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
        status: true,
        revokedAt: true,
        revocationPendingAt: true,
        updatedAt: true,
      },
    });
    const integrationId = existing?.id ?? crypto.randomUUID();
    const encryptedCredential = sealProviderCredential(integrationId, {
      ...token,
      scope: verifiedScope,
    });
    const now = new Date();
    let reconnectClaim: RevocationGateClaim | null = null;
    let reconnectRevoked = 0;
    if (existing?.status === "active" && !existing.revokedAt) {
      reconnectClaim = await claimRevocationGate({
        kind: "integration",
        organizationId: oauthState.organizationId,
        integrationId,
      });
      if (!reconnectClaim) {
        throw new Error("Another provider access change is already in progress");
      }
      let revocation;
      try {
        revocation = await revokeActiveLeases({
          organizationId: oauthState.organizationId,
          integrationId,
        });
      } catch (error) {
        await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
        throw error;
      }
      if (revocation.deferred > 0) {
        await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
        throw new Error("Active database access could not be revoked yet");
      }
      reconnectRevoked = revocation.revoked;
    } else if (existing?.revocationPendingAt) {
      throw new Error("Another provider access change is already in progress");
    }

    if (existing) {
      const updatePredicates = [
        eq(workspaceProviderIntegration.id, integrationId),
        eq(
          workspaceProviderIntegration.organizationId,
          oauthState.organizationId,
        ),
      ];
      if (reconnectClaim) {
        updatePredicates.push(
          eq(workspaceProviderIntegration.status, "active"),
          isNull(workspaceProviderIntegration.revokedAt),
          eq(
            workspaceProviderIntegration.revocationClaimId,
            reconnectClaim.claimId,
          ),
        );
      } else {
        updatePredicates.push(
          isNull(workspaceProviderIntegration.revocationPendingAt),
          isNull(workspaceProviderIntegration.revocationClaimId),
          eq(workspaceProviderIntegration.status, existing.status),
          eq(workspaceProviderIntegration.updatedAt, existing.updatedAt),
          existing.revokedAt
            ? eq(workspaceProviderIntegration.revokedAt, existing.revokedAt)
            : isNull(workspaceProviderIntegration.revokedAt),
        );
      }
      const integrationUpdate = db.update(workspaceProviderIntegration).set({
        status: "active",
        displayName: `PlanetScale · ${tokenInfo.subject.slice(-8)}`,
        encryptedCredential,
        credentialExpiresAt: new Date(token.expiresAt),
        grantedScope: verifiedScope,
        updatedAt: now,
        revokedAt: null,
        ...(reconnectClaim
          ? {}
          : {
              revocationPendingAt: null,
              revocationClaimedAt: null,
              revocationClaimId: null,
            }),
      }).where(and(...updatePredicates)).returning({
        id: workspaceProviderIntegration.id,
      });
      const bumpConnections = db.execute(sql`
        UPDATE ${workspaceConnection} AS connection
        SET "revision" = connection."revision" + 1,
            "updated_at" = ${now}
        FROM ${workspaceProviderIntegration} AS integration
        WHERE connection."organization_id" = ${oauthState.organizationId}
          AND connection."provider_integration_id" = integration."id"
          AND connection."deleted_at" IS NULL
          AND integration."id" = ${integrationId}::uuid
          AND integration."organization_id" = ${oauthState.organizationId}
          AND integration."updated_at" = ${now}
          ${reconnectClaim
            ? sql`AND integration."revocation_pending_at" IS NOT NULL
                  AND integration."revocation_claim_id" =
                    ${reconnectClaim.claimId}::uuid`
            : sql`AND integration."status" = 'active'
                  AND integration."revoked_at" IS NULL
                  AND integration."revocation_pending_at" IS NULL
                  AND integration."revocation_claim_id" IS NULL`}
      `);
      const auditEvent = db.execute(sql`
        INSERT INTO ${workspaceAuditEvent}
          ("organization_id", "actor_user_id", "action", "resource_type",
           "resource_id", "redacted_summary", "request_id")
        SELECT integration."organization_id", ${session.user.id},
               'provider.connect', 'provider_integration',
               integration."id"::text,
               jsonb_build_object(
                 'provider', 'planetScale',
                 'revokedLeases', ${reconnectRevoked}
               ),
               ${crypto.randomUUID()}::uuid
        FROM ${workspaceProviderIntegration} AS integration
        WHERE integration."id" = ${integrationId}::uuid
          AND integration."organization_id" = ${oauthState.organizationId}
          AND integration."updated_at" = ${now}
          ${reconnectClaim
            ? sql`AND integration."revocation_pending_at" IS NOT NULL
                  AND integration."revocation_claim_id" =
                    ${reconnectClaim.claimId}::uuid`
            : sql`AND integration."revocation_pending_at" IS NULL
                  AND integration."revocation_claim_id" IS NULL`}
      `);
      try {
        if (reconnectClaim) {
          const [updatedRows, , , clearedRows] = await db.batch([
            integrationUpdate,
            bumpConnections,
            auditEvent,
            db.update(workspaceProviderIntegration).set({
              revocationPendingAt: null,
              revocationClaimedAt: null,
              revocationClaimId: null,
            }).where(and(
              eq(workspaceProviderIntegration.id, integrationId),
              eq(
                workspaceProviderIntegration.organizationId,
                oauthState.organizationId,
              ),
              eq(workspaceProviderIntegration.updatedAt, now),
              eq(
                workspaceProviderIntegration.revocationClaimId,
                reconnectClaim.claimId,
              ),
            )).returning({ id: workspaceProviderIntegration.id }),
          ]);
          if (updatedRows.length !== 1 || clearedRows.length !== 1) {
            throw new Error("Provider access changed concurrently");
          }
        } else {
          const [updatedRows] = await db.batch([
            integrationUpdate,
            bumpConnections,
            auditEvent,
          ]);
          if (updatedRows.length !== 1) {
            throw new Error("Provider access changed concurrently");
          }
        }
      } catch (error) {
        if (reconnectClaim) {
          await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
        }
        throw error;
      }
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
          updatedAt: now,
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
