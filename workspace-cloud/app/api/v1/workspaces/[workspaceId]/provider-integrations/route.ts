// Workspace provider integration inventory and OAuth initiation. Secret material is
// omitted by explicit projection and OAuth state is single-use, hashed server data.
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { and, eq, inArray, isNull, lt, sql } from "drizzle-orm";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import {
  isUuid,
  jsonError,
  mutationAllowed,
  privateJson,
} from "../../../../../../lib/http";
import { providerCatalog } from "../../../../../../lib/provider-catalog";
import {
  parseManagedProviderResource,
  revokeActiveLeases,
} from "../../../../../../lib/provider-integrations";
import {
  claimRevocationGate,
  releaseRevocationGateClaim,
  type RevocationGateClaim,
} from "../../../../../../lib/revocation-gates";
import {
  isPlanetScaleConfigured,
  planetScaleAuthorizationUrl,
  PlanetScaleRequestError,
} from "../../../../../../lib/providers/planetscale";
import {
  inspectNeonCredential,
} from "../../../../../../lib/providers/neon";
import {
  gcpCloudSqlIntegrationIdentity,
  gcpCloudSqlPrincipalClaims,
  parseGcpCloudSqlCredential,
} from "../../../../../../lib/providers/gcp-cloud-sql-core";
import {
  validateGcpCloudSqlCredential,
  vercelOidcToken,
} from "../../../../../../lib/providers/gcp-cloud-sql";
import { ProviderRequestError } from "../../../../../../lib/providers/provider-types";
import {
  type NeonCredential,
} from "../../../../../../lib/providers/neon-core";
import {
  openProviderCredential,
  sealProviderCredential,
} from "../../../../../../lib/secret-envelope";
import {
  providerOauthState,
  workspaceAuditEvent,
  workspaceConnection,
  workspaceProviderIntegration,
  workspaceProviderPrincipalClaim,
} from "../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../lib/workspace-authorization";

type RouteContext = { params: Promise<{ workspaceId: string }> };

function sameSecret(left: string, right: string) {
  const leftBytes = Buffer.from(left, "utf8");
  const rightBytes = Buffer.from(right, "utf8");
  return leftBytes.length === rightBytes.length && timingSafeEqual(leftBytes, rightBytes);
}

function postgresErrorCode(error: unknown) {
  const seen = new Set<unknown>();
  let current = error;
  while (current && typeof current === "object" && !seen.has(current)) {
    seen.add(current);
    const record = current as { code?: unknown; cause?: unknown };
    if (typeof record.code === "string") return record.code;
    current = record.cause;
  }
  return null;
}

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
    const provider = integrations.find((item) => item.id === row.integrationId)?.provider;
    if (!provider) return [];
    try {
      return [{
        connectionId: row.connectionId,
        integrationId: row.integrationId,
        provider,
        resource: parseManagedProviderResource(provider, row.resource),
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
        : provider.id === "neon"
          ? true
          : provider.id === "gcpCloudSql"
            ? Boolean(vercelOidcToken(request))
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
  const body = (await request.json().catch(() => null)) as {
    provider?: unknown;
    configuration?: unknown;
  } | null;
  if (
    body?.provider !== "planetScale"
    && body?.provider !== "neon"
    && body?.provider !== "gcpCloudSql"
  ) {
    return jsonError("Managed access for this provider is not available", 409);
  }

  try {
    if (body.provider === "planetScale") {
      const state = randomBytes(32).toString("base64url");
      const stateHash = createHash("sha256").update(state).digest("base64url");
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
    }

    let credential: NeonCredential | ReturnType<typeof parseGcpCloudSqlCredential>;
    let externalAccountId: string;
    let displayName: string;
    let grantedScope: string;
    let neonConfigurationCredential: NeonCredential | null = null;
    let gcpIdentity:
      | ReturnType<typeof gcpCloudSqlIntegrationIdentity>
      | null = null;
    if (body.provider === "neon") {
      const configuration = body.configuration as Record<string, unknown> | null;
      const apiKey = typeof configuration?.apiKey === "string"
        ? configuration.apiKey.trim()
        : "";
      const organizationId = typeof configuration?.organizationId === "string"
        && configuration.organizationId.trim()
        ? configuration.organizationId.trim()
        : null;
      if (
        apiKey.length < 20
        || apiKey.length > 512
        || /\s/.test(apiKey)
        || (organizationId !== null
          && !/^[a-z0-9][a-z0-9-]{0,59}$/.test(organizationId))
      ) {
        return jsonError("Invalid Neon API key configuration", 400);
      }
      credential = { apiKey, organizationId };
      neonConfigurationCredential = credential;
      const info = await inspectNeonCredential(credential);
      externalAccountId = info.externalAccountId;
      displayName = info.displayName;
      grantedScope = `projects:${info.projectCount}:${info.scopeFingerprint.slice(0, 16)}`;
    } else {
      try {
        credential = parseGcpCloudSqlCredential(body.configuration);
      } catch {
        return jsonError("Invalid GCP trust configuration", 400);
      }
      const oidcToken = vercelOidcToken(request);
      if (!oidcToken) {
        return jsonError("Vercel OIDC is not enabled for this deployment", 503);
      }
      await validateGcpCloudSqlCredential(credential, oidcToken);
      gcpIdentity = gcpCloudSqlIntegrationIdentity(credential);
      externalAccountId = gcpIdentity.externalAccountId;
      displayName = `GCP Cloud SQL · ${credential.projectId} / ${credential.instanceId}`;
      grantedScope = credential.writeServiceAccountEmail
        ? "cloudsql.read cloudsql.write"
        : "cloudsql.read";
    }

    const provider = body.provider;
    type ExistingIntegration = {
      id: string;
      status: string;
      revokedAt: Date | null;
      revocationPendingAt: Date | null;
      updatedAt: Date;
    };
    let existing: ExistingIntegration | undefined;
    if (provider === "gcpCloudSql" && gcpIdentity) {
      const principalClaims = gcpCloudSqlPrincipalClaims(gcpIdentity);
      const claimedPrincipals = await db.select({
        principalFingerprint:
          workspaceProviderPrincipalClaim.principalFingerprint,
        targetFingerprint: workspaceProviderPrincipalClaim.targetFingerprint,
        integrationId: workspaceProviderIntegration.id,
        organizationId: workspaceProviderIntegration.organizationId,
        provider: workspaceProviderIntegration.provider,
        status: workspaceProviderIntegration.status,
        revokedAt: workspaceProviderIntegration.revokedAt,
        revocationPendingAt: workspaceProviderIntegration.revocationPendingAt,
        updatedAt: workspaceProviderIntegration.updatedAt,
      }).from(workspaceProviderPrincipalClaim).innerJoin(
        workspaceProviderIntegration,
        eq(
          workspaceProviderPrincipalClaim.integrationId,
          workspaceProviderIntegration.id,
        ),
      ).where(inArray(
        workspaceProviderPrincipalClaim.principalFingerprint,
        principalClaims.map((claim) => claim.principalFingerprint),
      ));
      if (claimedPrincipals.some((row) => (
        row.organizationId !== workspaceId
        || row.provider !== "gcpCloudSql"
        || row.status !== "active"
        || row.revokedAt !== null
        || row.targetFingerprint !== gcpIdentity.instance
      ))) {
        return jsonError(
          "Each Cloud SQL instance must use dedicated service accounts",
          409,
        );
      }
      const principalIntegrationIds = new Set(
        claimedPrincipals.map((row) => row.integrationId),
      );
      if (principalIntegrationIds.size > 1) {
        return jsonError(
          "Each Cloud SQL instance must use dedicated service accounts",
          409,
        );
      }
      const targetRows = await db.select({
        id: workspaceProviderIntegration.id,
        status: workspaceProviderIntegration.status,
        revokedAt: workspaceProviderIntegration.revokedAt,
        revocationPendingAt: workspaceProviderIntegration.revocationPendingAt,
        updatedAt: workspaceProviderIntegration.updatedAt,
      }).from(workspaceProviderPrincipalClaim).innerJoin(
        workspaceProviderIntegration,
        eq(
          workspaceProviderPrincipalClaim.integrationId,
          workspaceProviderIntegration.id,
        ),
      ).where(and(
        eq(workspaceProviderPrincipalClaim.targetFingerprint, gcpIdentity.instance),
        eq(workspaceProviderIntegration.organizationId, workspaceId),
        eq(workspaceProviderIntegration.provider, "gcpCloudSql"),
        eq(workspaceProviderIntegration.status, "active"),
        isNull(workspaceProviderIntegration.revokedAt),
      ));
      const targetIntegrations = new Map(
        targetRows.map((row) => [row.id, row]),
      );
      if (targetIntegrations.size > 1) {
        return jsonError(
          "Cloud SQL target is already connected more than once",
          409,
        );
      }
      const principalIntegrationId = [...principalIntegrationIds][0];
      const targetIntegration = [...targetIntegrations.values()][0];
      if (
        principalIntegrationId
        && targetIntegration
        && principalIntegrationId !== targetIntegration.id
      ) {
        return jsonError(
          "Each Cloud SQL instance must use dedicated service accounts",
          409,
        );
      }
      if (targetIntegration) {
        existing = targetIntegration;
      } else if (principalIntegrationId) {
        const principalIntegration = claimedPrincipals.find(
          (row) => row.integrationId === principalIntegrationId,
        );
        if (principalIntegration) {
          existing = {
            id: principalIntegration.integrationId,
            status: principalIntegration.status,
            revokedAt: principalIntegration.revokedAt,
            revocationPendingAt:
              principalIntegration.revocationPendingAt,
            updatedAt: principalIntegration.updatedAt,
          };
        }
      }
      if (!existing) {
        existing = await db.query.workspaceProviderIntegration.findFirst({
          where: and(
            eq(workspaceProviderIntegration.organizationId, workspaceId),
            eq(workspaceProviderIntegration.provider, "gcpCloudSql"),
            eq(
              workspaceProviderIntegration.externalAccountId,
              externalAccountId,
            ),
          ),
          columns: {
            id: true,
            status: true,
            revokedAt: true,
            revocationPendingAt: true,
            updatedAt: true,
          },
        });
      }
    } else {
      existing = await db.query.workspaceProviderIntegration.findFirst({
        where: and(
          eq(workspaceProviderIntegration.organizationId, workspaceId),
          eq(workspaceProviderIntegration.provider, provider),
          eq(workspaceProviderIntegration.externalAccountId, externalAccountId),
        ),
        columns: {
          id: true,
          status: true,
          revokedAt: true,
          revocationPendingAt: true,
          updatedAt: true,
        },
      });
      if (!existing && provider === "neon") {
        // One-time compatibility path for integrations created before Neon replaced
        // /auth with scoped user/organization identity. The secret never leaves this
        // server-side comparison and the row is rewritten to the v2 identity below.
        const legacyRows = await db.select({
          id: workspaceProviderIntegration.id,
          externalAccountId: workspaceProviderIntegration.externalAccountId,
          encryptedCredential: workspaceProviderIntegration.encryptedCredential,
          status: workspaceProviderIntegration.status,
          revokedAt: workspaceProviderIntegration.revokedAt,
          revocationPendingAt: workspaceProviderIntegration.revocationPendingAt,
          updatedAt: workspaceProviderIntegration.updatedAt,
        }).from(workspaceProviderIntegration).where(and(
          eq(workspaceProviderIntegration.organizationId, workspaceId),
          eq(workspaceProviderIntegration.provider, "neon"),
          eq(workspaceProviderIntegration.status, "active"),
          isNull(workspaceProviderIntegration.revokedAt),
        ));
        existing = legacyRows.find((row) => {
          if (row.externalAccountId.startsWith("neon:v2:")) return false;
          try {
            const stored = openProviderCredential<NeonCredential>(
              row.id,
              row.encryptedCredential,
            );
            return neonConfigurationCredential !== null
              && sameSecret(stored.apiKey, neonConfigurationCredential.apiKey);
          } catch {
            return false;
          }
        });
      }
    }
    const integrationId = existing?.id ?? crypto.randomUUID();
    const encryptedCredential = sealProviderCredential(integrationId, credential);
    const now = new Date();
    let reconnectClaim: RevocationGateClaim | null = null;
    let reconnectRevoked = 0;
    if (existing?.status === "active" && !existing.revokedAt) {
      reconnectClaim = await claimRevocationGate({
        kind: "integration",
        organizationId: workspaceId,
        integrationId,
      });
      if (!reconnectClaim) {
        return jsonError("Another provider access change is already in progress", 409);
      }
      let revocation;
      try {
        revocation = await revokeActiveLeases({
          organizationId: workspaceId,
          integrationId,
        });
      } catch (error) {
        await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
        throw error;
      }
      if (revocation.deferred > 0) {
        await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
        return jsonError(
          "Active database access could not be revoked yet. Retry reconnecting.",
          409,
        );
      }
      reconnectRevoked = revocation.revoked;
    } else if (existing?.revocationPendingAt) {
      return jsonError("Another provider access change is already in progress", 409);
    }

    if (existing && provider === "gcpCloudSql" && gcpIdentity) {
      const principalClaims = gcpCloudSqlPrincipalClaims(gcpIdentity);
      const claimValues = sql.join(
        principalClaims.map((claim) => sql`(
          ${claim.principalFingerprint},
          ${claim.targetFingerprint},
          ${claim.accessKind}
        )`),
        sql`, `,
      );
      if (reconnectClaim) {
        const [updatedRows, , , , clearedRows] = await db.batch([
          db.update(workspaceProviderIntegration).set({
            status: "active",
            externalAccountId,
            displayName,
            encryptedCredential,
            credentialExpiresAt: null,
            grantedScope,
            revokedAt: null,
            updatedAt: now,
          }).where(and(
            eq(workspaceProviderIntegration.id, integrationId),
            eq(workspaceProviderIntegration.organizationId, workspaceId),
            eq(workspaceProviderIntegration.status, "active"),
            isNull(workspaceProviderIntegration.revokedAt),
            eq(
              workspaceProviderIntegration.revocationClaimId,
              reconnectClaim.claimId,
            ),
          )).returning({ id: workspaceProviderIntegration.id }),
          db.execute(sql`
            DELETE FROM ${workspaceProviderPrincipalClaim} AS claim
            USING ${workspaceProviderIntegration} AS integration
            WHERE claim."integration_id" = integration."id"
              AND integration."id" = ${integrationId}::uuid
              AND integration."organization_id" = ${workspaceId}
              AND integration."updated_at" = ${now}
              AND integration."revocation_claim_id" =
                ${reconnectClaim.claimId}::uuid
          `),
          db.execute(sql`
            INSERT INTO ${workspaceProviderPrincipalClaim}
              ("principal_fingerprint", "organization_id", "integration_id",
               "target_fingerprint", "access_kind", "created_at", "updated_at")
            SELECT candidate."principal_fingerprint",
                   integration."organization_id",
                   integration."id",
                   candidate."target_fingerprint",
                   candidate."access_kind",
                   ${now},
                   ${now}
            FROM (VALUES ${claimValues}) AS candidate(
              "principal_fingerprint", "target_fingerprint", "access_kind"
            )
            INNER JOIN ${workspaceProviderIntegration} AS integration
              ON integration."id" = ${integrationId}::uuid
             AND integration."organization_id" = ${workspaceId}
             AND integration."updated_at" = ${now}
             AND integration."revocation_claim_id" =
               ${reconnectClaim.claimId}::uuid
          `),
          db.execute(sql`
            INSERT INTO ${workspaceAuditEvent}
              ("organization_id", "actor_user_id", "action", "resource_type",
               "resource_id", "redacted_summary", "request_id")
            SELECT integration."organization_id",
                   ${authorization.session.user.id},
                   'provider.connect',
                   'provider_integration',
                   integration."id"::text,
                   jsonb_build_object(
                     'provider', ${provider},
                     'revokedLeases', ${reconnectRevoked}
                   ),
                   ${crypto.randomUUID()}::uuid
            FROM ${workspaceProviderIntegration} AS integration
            WHERE integration."id" = ${integrationId}::uuid
              AND integration."organization_id" = ${workspaceId}
              AND integration."updated_at" = ${now}
              AND integration."revocation_claim_id" =
                ${reconnectClaim.claimId}::uuid
          `),
          db.update(workspaceProviderIntegration).set({
            revocationPendingAt: null,
            revocationClaimedAt: null,
            revocationClaimId: null,
          }).where(and(
            eq(workspaceProviderIntegration.id, integrationId),
            eq(workspaceProviderIntegration.organizationId, workspaceId),
            eq(workspaceProviderIntegration.updatedAt, now),
            eq(
              workspaceProviderIntegration.revocationClaimId,
              reconnectClaim.claimId,
            ),
          )).returning({ id: workspaceProviderIntegration.id }),
        ]).catch(async (error) => {
          await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
          throw error;
        });
        if (updatedRows.length !== 1 || clearedRows.length !== 1) {
          await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
          return jsonError(
            "Provider access changed concurrently. Retry connecting.",
            409,
          );
        }
      } else {
        const priorRevokedGuard = existing.revokedAt
          ? sql`integration."revoked_at" = ${existing.revokedAt}`
          : sql`integration."revoked_at" IS NULL`;
        const result = await db.execute<{ id: string }>(sql`
          WITH updated_integration AS (
            UPDATE ${workspaceProviderIntegration} AS integration
            SET "status" = 'active',
                "external_account_id" = ${externalAccountId},
                "display_name" = ${displayName},
                "encrypted_credential" = ${encryptedCredential},
                "credential_expires_at" = NULL,
                "granted_scope" = ${grantedScope},
                "revoked_at" = NULL,
                "revocation_pending_at" = NULL,
                "revocation_claimed_at" = NULL,
                "revocation_claim_id" = NULL,
                "updated_at" = ${now}
            WHERE integration."id" = ${integrationId}::uuid
              AND integration."organization_id" = ${workspaceId}
              AND integration."status" = ${existing.status}
              AND integration."updated_at" = ${existing.updatedAt}
              AND integration."revocation_pending_at" IS NULL
              AND ${priorRevokedGuard}
            RETURNING integration."id", integration."organization_id"
          ),
          inserted_claims AS (
            INSERT INTO ${workspaceProviderPrincipalClaim}
              ("principal_fingerprint", "organization_id", "integration_id",
               "target_fingerprint", "access_kind", "created_at", "updated_at")
            SELECT candidate."principal_fingerprint",
                   updated_integration."organization_id",
                   updated_integration."id",
                   candidate."target_fingerprint",
                   candidate."access_kind",
                   ${now},
                   ${now}
            FROM (VALUES ${claimValues}) AS candidate(
              "principal_fingerprint", "target_fingerprint", "access_kind"
            )
            CROSS JOIN updated_integration
            RETURNING "principal_fingerprint"
          ),
          audit_event AS (
            INSERT INTO ${workspaceAuditEvent}
              ("organization_id", "actor_user_id", "action", "resource_type",
               "resource_id", "redacted_summary", "request_id")
            SELECT updated_integration."organization_id",
                   ${authorization.session.user.id},
                   'provider.connect',
                   'provider_integration',
                   updated_integration."id"::text,
                   jsonb_build_object(
                     'provider', ${provider},
                     'revokedLeases', ${reconnectRevoked}
                   ),
                   ${crypto.randomUUID()}::uuid
            FROM updated_integration
            RETURNING "resource_id"
          )
          SELECT "id"::text AS "id" FROM updated_integration
        `);
        if (result.rows.length !== 1) {
          return jsonError(
            "Provider access changed concurrently. Retry connecting.",
            409,
          );
        }
      }
    } else if (existing) {
      const updatePredicates = [
        eq(workspaceProviderIntegration.id, integrationId),
        eq(workspaceProviderIntegration.organizationId, workspaceId),
      ];
      if (reconnectClaim) {
        updatePredicates.push(
          eq(workspaceProviderIntegration.status, "active"),
          isNull(workspaceProviderIntegration.revokedAt),
          eq(workspaceProviderIntegration.revocationClaimId, reconnectClaim.claimId),
        );
      } else {
        updatePredicates.push(
          isNull(workspaceProviderIntegration.revocationPendingAt),
          eq(workspaceProviderIntegration.status, existing.status),
          eq(workspaceProviderIntegration.updatedAt, existing.updatedAt),
          existing.revokedAt
            ? eq(workspaceProviderIntegration.revokedAt, existing.revokedAt)
            : isNull(workspaceProviderIntegration.revokedAt),
        );
      }
      const [updatedRows] = await db.batch([
        db.update(workspaceProviderIntegration).set({
          status: "active",
          externalAccountId,
          displayName,
          encryptedCredential,
          credentialExpiresAt: null,
          grantedScope,
          revokedAt: null,
          revocationPendingAt: null,
          revocationClaimedAt: null,
          revocationClaimId: null,
          updatedAt: now,
        }).where(and(...updatePredicates)).returning({
          id: workspaceProviderIntegration.id,
        }),
        db.execute(sql`
          INSERT INTO ${workspaceAuditEvent}
            ("organization_id", "actor_user_id", "action", "resource_type",
             "resource_id", "redacted_summary", "request_id")
          SELECT integration."organization_id", ${authorization.session.user.id},
                 'provider.connect', 'provider_integration',
                 integration."id"::text,
                 jsonb_build_object(
                   'provider', ${provider},
                   'revokedLeases', ${reconnectRevoked}
                 ),
                 ${crypto.randomUUID()}::uuid
          FROM ${workspaceProviderIntegration} AS integration
          WHERE integration."id" = ${integrationId}::uuid
            AND integration."organization_id" = ${workspaceId}
            AND integration."updated_at" = ${now}
            AND integration."revocation_pending_at" IS NULL
        `),
      ]).catch(async (error) => {
        if (reconnectClaim) {
          await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
        }
        throw error;
      });
      if (updatedRows.length !== 1) {
        if (reconnectClaim) {
          await releaseRevocationGateClaim(reconnectClaim).catch(() => false);
        }
        return jsonError("Provider access changed concurrently. Retry connecting.", 409);
      }
    } else {
      const integrationInsert = db.insert(workspaceProviderIntegration).values({
        id: integrationId,
        organizationId: workspaceId,
        provider,
        externalAccountId,
        displayName,
        encryptedCredential,
        credentialExpiresAt: null,
        grantedScope,
        createdByUserId: authorization.session.user.id,
      });
      const auditInsert = db.insert(workspaceAuditEvent).values({
        organizationId: workspaceId,
        actorUserId: authorization.session.user.id,
        action: "provider.connect",
        resourceType: "provider_integration",
        resourceId: integrationId,
        redactedSummary: { provider },
        requestId: crypto.randomUUID(),
      });
      if (provider === "gcpCloudSql" && gcpIdentity) {
        await db.batch([
          integrationInsert,
          db.insert(workspaceProviderPrincipalClaim).values(
            gcpCloudSqlPrincipalClaims(gcpIdentity).map((claim) => ({
              ...claim,
              organizationId: workspaceId,
              integrationId,
              createdAt: now,
              updatedAt: now,
            })),
          ),
          auditInsert,
        ]);
      } else {
        await db.batch([integrationInsert, auditInsert]);
      }
    }
    return privateJson({
      integration: {
        id: integrationId,
        provider,
        displayName,
        grantedScope,
        updatedAt: now.toISOString(),
      },
    }, { status: existing ? 200 : 201 });
  } catch (error) {
    if (body.provider === "gcpCloudSql" && postgresErrorCode(error) === "23505") {
      return jsonError(
        "Cloud SQL service accounts or target are already connected",
        409,
      );
    }
    if (
      error instanceof PlanetScaleRequestError
      || error instanceof ProviderRequestError
    ) {
      return jsonError(error.message, error.status);
    }
    return jsonError("Provider connection could not be verified", 502);
  }
}
