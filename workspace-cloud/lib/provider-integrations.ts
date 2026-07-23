// Server-side provider integration and lease lifecycle. This module is the only
// database-facing code allowed to decrypt provider authorization credentials.
import "server-only";

import { and, eq, gt, inArray, isNull } from "drizzle-orm";
import { db } from "./db";
import {
  workspaceConnection,
  workspaceCredentialLease,
  workspaceProviderIntegration,
} from "./schema";
import { openProviderCredential, sealProviderCredential } from "./secret-envelope";
import {
  issuePlanetScaleLease,
  PlanetScaleRequestError,
  refreshPlanetScaleToken,
  revokePlanetScaleAuthorization,
  revokePlanetScaleLease,
  type PlanetScaleLease,
  type PlanetScaleResource,
  type PlanetScaleToken,
} from "./providers/planetscale";
import { missingPlanetScaleManagedScopes } from "./providers/planetscale-core";

export type ActiveProviderIntegration = {
  id: string;
  organizationId: string;
  provider: string;
  encryptedCredential: string;
  credentialExpiresAt: Date | null;
};

export type LeaseRevocationFilter = {
  organizationId: string;
  userId?: string;
  connectionId?: string;
  integrationId?: string;
};

export type LeaseRevocationResult = {
  revoked: number;
  deferred: number;
};

function isSegment(value: unknown): value is string {
  return typeof value === "string"
    && /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(value);
}

export function parsePlanetScaleResource(value: unknown): PlanetScaleResource {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("PlanetScale resource is required");
  }
  const body = value as Record<string, unknown>;
  if (
    !isSegment(body.organization)
    || !isSegment(body.database)
    || !isSegment(body.branch)
    || (body.engine !== "postgres" && body.engine !== "mysql")
  ) {
    throw new Error("Invalid PlanetScale resource");
  }
  return {
    organization: body.organization,
    database: body.database,
    branch: body.branch,
    engine: body.engine,
  };
}

export async function activeProviderIntegration(
  organizationId: string,
  integrationId: string,
): Promise<ActiveProviderIntegration | null> {
  const row = await db.query.workspaceProviderIntegration.findFirst({
    where: and(
      eq(workspaceProviderIntegration.id, integrationId),
      eq(workspaceProviderIntegration.organizationId, organizationId),
      eq(workspaceProviderIntegration.status, "active"),
      isNull(workspaceProviderIntegration.revokedAt),
    ),
    columns: {
      id: true,
      organizationId: true,
      provider: true,
      encryptedCredential: true,
      credentialExpiresAt: true,
    },
  });
  return row ?? null;
}

export async function providerAccessToken(
  integration: ActiveProviderIntegration,
): Promise<string> {
  if (integration.provider !== "planetScale") {
    throw new Error("Managed credential provider is not available");
  }
  const credential = openProviderCredential<PlanetScaleToken>(
    integration.id,
    integration.encryptedCredential,
  );
  if (missingPlanetScaleManagedScopes(credential.scope).length > 0) {
    throw new PlanetScaleRequestError(
      "PlanetScale authorization is missing required managed-access scopes",
      403,
    );
  }
  const expiresAt = new Date(credential.expiresAt);
  if (
    credential.accessToken
    && credential.refreshToken
    && !Number.isNaN(expiresAt.valueOf())
    && expiresAt.valueOf() > Date.now() + 2 * 60 * 1_000
  ) {
    return credential.accessToken;
  }

  const refreshed = await refreshPlanetScaleToken(
    credential.refreshToken,
    credential.scope,
  );
  if (missingPlanetScaleManagedScopes(refreshed.scope).length > 0) {
    throw new PlanetScaleRequestError(
      "PlanetScale authorization lost required managed-access scopes",
      403,
    );
  }
  const encryptedCredential = sealProviderCredential(integration.id, refreshed);
  await db.update(workspaceProviderIntegration)
    .set({
      encryptedCredential,
      credentialExpiresAt: new Date(refreshed.expiresAt),
      grantedScope: refreshed.scope,
      updatedAt: new Date(),
    })
    .where(and(
      eq(workspaceProviderIntegration.id, integration.id),
      eq(workspaceProviderIntegration.status, "active"),
    ));
  integration.encryptedCredential = encryptedCredential;
  integration.credentialExpiresAt = new Date(refreshed.expiresAt);
  return refreshed.accessToken;
}

export async function revokeProviderAuthorization(
  integration: ActiveProviderIntegration,
) {
  if (integration.provider !== "planetScale") {
    throw new Error("Managed credential provider is not available");
  }
  const credential = openProviderCredential<PlanetScaleToken>(
    integration.id,
    integration.encryptedCredential,
  );
  await revokePlanetScaleAuthorization(credential.refreshToken);
}

export async function issueManagedLease(input: {
  organizationId: string;
  connectionId: string;
  userId: string;
  accessMode: "read" | "write";
  integration: ActiveProviderIntegration;
  resource: PlanetScaleResource;
}): Promise<PlanetScaleLease & { leaseId: string }> {
  if (input.integration.provider !== "planetScale") {
    throw new Error("Managed credential provider is not available");
  }
  const leaseId = crypto.randomUUID();
  const label = `dopedb-${input.userId.replace(/-/g, "").slice(0, 8)}-${
    leaseId.replace(/-/g, "").slice(0, 8)
  }`;

  // Reserve the audit index before the external API call. Connection changes and
  // provider disconnects see the pending row and wait instead of racing past an
  // in-flight credential that does not have an external id yet.
  await db.insert(workspaceCredentialLease).values({
    id: leaseId,
    organizationId: input.organizationId,
    connectionId: input.connectionId,
    integrationId: input.integration.id,
    userId: input.userId,
    provider: input.integration.provider,
    accessMode: input.accessMode,
    externalCredentialId: leaseId,
    externalCredentialKind: "pending",
    expiresAt: new Date(Date.now() + 2 * 60 * 1_000),
  });

  let accessToken = "";
  let lease: PlanetScaleLease;
  try {
    accessToken = await providerAccessToken(input.integration);
    lease = await issuePlanetScaleLease(
      accessToken,
      input.resource,
      input.accessMode,
      label,
    );
  } catch (error) {
    await db.update(workspaceCredentialLease)
      .set({ revokedAt: new Date() })
      .where(eq(workspaceCredentialLease.id, leaseId))
      .catch(() => undefined);
    throw error;
  }

  try {
    const updated = await db.update(workspaceCredentialLease).set({
      externalCredentialId: lease.externalCredentialId,
      externalCredentialKind: lease.externalCredentialKind,
      expiresAt: new Date(lease.expiresAt),
    }).where(and(
      eq(workspaceCredentialLease.id, leaseId),
      eq(workspaceCredentialLease.externalCredentialKind, "pending"),
      isNull(workspaceCredentialLease.revokedAt),
    )).returning({ id: workspaceCredentialLease.id });
    if (updated.length !== 1) throw new Error("Managed lease reservation is no longer active");
  } catch (error) {
    await revokePlanetScaleLease(
      accessToken,
      input.resource,
      lease.externalCredentialKind,
      lease.externalCredentialId,
    ).catch(() => undefined);
    await db.update(workspaceCredentialLease)
      .set({ revokedAt: new Date() })
      .where(eq(workspaceCredentialLease.id, leaseId))
      .catch(() => undefined);
    throw error;
  }
  return { ...lease, leaseId };
}

export async function revokeActiveLeases(
  filter: LeaseRevocationFilter,
): Promise<LeaseRevocationResult> {
  const predicates = [
    eq(workspaceCredentialLease.organizationId, filter.organizationId),
    isNull(workspaceCredentialLease.revokedAt),
    gt(workspaceCredentialLease.expiresAt, new Date()),
  ];
  if (filter.userId) predicates.push(eq(workspaceCredentialLease.userId, filter.userId));
  if (filter.connectionId) {
    predicates.push(eq(workspaceCredentialLease.connectionId, filter.connectionId));
  }
  if (filter.integrationId) {
    predicates.push(eq(workspaceCredentialLease.integrationId, filter.integrationId));
  }
  const leases = await db.select({
    id: workspaceCredentialLease.id,
    integrationId: workspaceCredentialLease.integrationId,
    credentialId: workspaceCredentialLease.externalCredentialId,
    credentialKind: workspaceCredentialLease.externalCredentialKind,
    providerResource: workspaceConnection.providerResource,
  }).from(workspaceCredentialLease)
    .innerJoin(
      workspaceConnection,
      eq(workspaceCredentialLease.connectionId, workspaceConnection.id),
    )
    .where(and(...predicates));
  if (leases.length === 0) return { revoked: 0, deferred: 0 };

  const integrationIds = [...new Set(leases.map((item) => item.integrationId))];
  const integrations = await db.select({
    id: workspaceProviderIntegration.id,
    organizationId: workspaceProviderIntegration.organizationId,
    provider: workspaceProviderIntegration.provider,
    encryptedCredential: workspaceProviderIntegration.encryptedCredential,
    credentialExpiresAt: workspaceProviderIntegration.credentialExpiresAt,
  }).from(workspaceProviderIntegration).where(and(
    inArray(workspaceProviderIntegration.id, integrationIds),
    eq(workspaceProviderIntegration.status, "active"),
    isNull(workspaceProviderIntegration.revokedAt),
  ));
  const integrationMap = new Map(integrations.map((item) => [item.id, item]));
  let revoked = 0;
  let deferred = 0;

  for (const lease of leases) {
    const integration = integrationMap.get(lease.integrationId);
    try {
      if (
        !integration
        || integration.provider !== "planetScale"
        || (lease.credentialKind !== "role" && lease.credentialKind !== "password")
      ) {
        throw new Error("Lease provider is unavailable");
      }
      const resource = parsePlanetScaleResource(lease.providerResource);
      const token = await providerAccessToken(integration);
      await revokePlanetScaleLease(
        token,
        resource,
        lease.credentialKind,
        lease.credentialId,
      );
      await db.update(workspaceCredentialLease)
        .set({ revokedAt: new Date() })
        .where(and(
          eq(workspaceCredentialLease.id, lease.id),
          isNull(workspaceCredentialLease.revokedAt),
        ));
      revoked += 1;
    } catch (error) {
      if (error instanceof PlanetScaleRequestError && error.status === 404) {
        await db.update(workspaceCredentialLease)
          .set({ revokedAt: new Date() })
          .where(and(
            eq(workspaceCredentialLease.id, lease.id),
            isNull(workspaceCredentialLease.revokedAt),
          ));
        revoked += 1;
        continue;
      }
      // TTL remains the hard backstop. Callers audit deferred revocations and can
      // retry without retaining a database password or exposing provider details.
      deferred += 1;
    }
  }
  return { revoked, deferred };
}
