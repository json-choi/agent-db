// Server-side provider integration and lease lifecycle. This module is the only
// database-facing code allowed to decrypt provider authorization credentials.
import "server-only";

import { and, asc, eq, inArray, isNotNull, isNull, sql } from "drizzle-orm";
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
  validatePlanetScaleResource,
  listPlanetScaleBranches,
  listPlanetScaleDatabases,
  listPlanetScaleOrganizations,
  type PlanetScaleResource,
  type PlanetScaleToken,
} from "./providers/planetscale";
import { missingPlanetScaleManagedScopes } from "./providers/planetscale-core";
import {
  issueNeonLease,
  listNeonBranches,
  listNeonDatabases,
  listNeonProjects,
  neonRoleForLease,
  revokeNeonLease,
  validateNeonResource,
} from "./providers/neon";
import {
  parseNeonResource,
  type NeonCredential,
  type NeonResource,
} from "./providers/neon-core";
import {
  issueGcpCloudSqlLease,
  listGcpCloudSqlDatabases,
  listGcpCloudSqlInstances,
  listGcpProjects,
  validateGcpCloudSqlResource,
} from "./providers/gcp-cloud-sql";
import {
  parseGcpCloudSqlResource,
  type GcpCloudSqlCredential,
  type GcpCloudSqlResource,
} from "./providers/gcp-cloud-sql-core";
import {
  ProviderRequestError,
  type ManagedProviderLease,
  type ProviderResourceItem,
} from "./providers/provider-types";
import {
  finalizeManagedLeaseIfUnblocked,
  reserveManagedLeaseIfUnblocked,
  type ManagedLeaseAuthority,
} from "./revocation-gates";
import type { WorkspaceRoleName } from "./workspace-permissions";

export type ActiveProviderIntegration = {
  id: string;
  organizationId: string;
  provider: string;
  encryptedCredential: string;
  credentialExpiresAt: Date | null;
};

export type ManagedProviderResource =
  | PlanetScaleResource
  | NeonResource
  | GcpCloudSqlResource;

export type LeaseRevocationFilter = {
  organizationId: string;
  leaseId?: string;
  userId?: string;
  connectionId?: string;
  integrationId?: string;
};

export type LeaseRevocationResult = {
  revoked: number;
  deferred: number;
};

export type ExpiredLeaseCleanupResult = LeaseRevocationResult & {
  scanned: number;
};

const CLEANUP_CLAIM_STALE_SECONDS = 2 * 60;
const CLEANUP_RETRY_BASE_MS = 60 * 1_000;
const CLEANUP_RETRY_MAX_MS = 60 * 60 * 1_000;

export function managedLeaseCleanupRetryDelayMs(attempt: number) {
  if (!Number.isInteger(attempt) || attempt < 1) {
    throw new Error("Invalid managed lease cleanup attempt");
  }
  return Math.min(
    CLEANUP_RETRY_BASE_MS * (2 ** Math.min(attempt - 1, 16)),
    CLEANUP_RETRY_MAX_MS,
  );
}

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

export function parseManagedProviderResource(
  provider: string,
  value: unknown,
): ManagedProviderResource {
  switch (provider) {
    case "planetScale":
      return parsePlanetScaleResource(value);
    case "neon":
      return parseNeonResource(value);
    case "gcpCloudSql":
      return parseGcpCloudSqlResource(value);
    default:
      throw new Error("Managed credential provider is not available");
  }
}

async function providerIntegration(
  organizationId: string,
  integrationId: string,
  allowPendingRevocation: boolean,
): Promise<ActiveProviderIntegration | null> {
  const predicates = [
    eq(workspaceProviderIntegration.id, integrationId),
    eq(workspaceProviderIntegration.organizationId, organizationId),
    eq(workspaceProviderIntegration.status, "active"),
    isNull(workspaceProviderIntegration.revokedAt),
  ];
  if (!allowPendingRevocation) {
    predicates.push(isNull(workspaceProviderIntegration.revocationPendingAt));
  }
  const row = await db.query.workspaceProviderIntegration.findFirst({
    where: and(...predicates),
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

export function activeProviderIntegration(
  organizationId: string,
  integrationId: string,
) {
  return providerIntegration(organizationId, integrationId, false);
}

export function providerIntegrationForRevocation(
  organizationId: string,
  integrationId: string,
) {
  return providerIntegration(organizationId, integrationId, true);
}

export async function providerAccessToken(
  integration: ActiveProviderIntegration,
): Promise<string> {
  if (integration.provider !== "planetScale") {
    throw new Error("PlanetScale access token requested for another provider");
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

function neonCredential(integration: ActiveProviderIntegration) {
  return openProviderCredential<NeonCredential>(
    integration.id,
    integration.encryptedCredential,
  );
}

function gcpCredential(integration: ActiveProviderIntegration) {
  return openProviderCredential<GcpCloudSqlCredential>(
    integration.id,
    integration.encryptedCredential,
  );
}

function requiredOidcToken(value: string | null | undefined) {
  if (!value) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Vercel OIDC is not available for GCP federation",
      503,
    );
  }
  return value;
}

export async function revokeProviderAuthorization(
  integration: ActiveProviderIntegration,
) {
  if (integration.provider === "planetScale") {
    const credential = openProviderCredential<PlanetScaleToken>(
      integration.id,
      integration.encryptedCredential,
    );
    await revokePlanetScaleAuthorization(credential.refreshToken);
    return;
  }
  if (integration.provider === "neon" || integration.provider === "gcpCloudSql") {
    // Neon API keys and GCP trust are customer-owned and may be shared by another
    // workspace. Disconnect scrubs our encrypted copy without deleting that trust.
    return;
  }
  throw new Error("Managed credential provider is not available");
}

export async function discoverProviderResources(input: {
  integration: ActiveProviderIntegration;
  kind: string;
  selection: Record<string, string>;
  oidcToken?: string | null;
}): Promise<ProviderResourceItem[]> {
  const { integration, kind, selection } = input;
  switch (integration.provider) {
    case "planetScale": {
      const token = await providerAccessToken(integration);
      if (kind === "organizations") return listPlanetScaleOrganizations(token);
      if (kind === "databases" && isSegment(selection.organization)) {
        return listPlanetScaleDatabases(token, selection.organization);
      }
      if (
        kind === "branches"
        && isSegment(selection.organization)
        && isSegment(selection.database)
      ) {
        return listPlanetScaleBranches(
          token,
          selection.organization,
          selection.database,
        );
      }
      break;
    }
    case "neon": {
      const credential = neonCredential(integration);
      if (kind === "projects") return listNeonProjects(credential);
      if (kind === "branches" && isSegment(selection.project)) {
        return listNeonBranches(credential, selection.project);
      }
      if (
        kind === "databases"
        && isSegment(selection.project)
        && isSegment(selection.branch)
      ) {
        return listNeonDatabases(
          credential,
          selection.project,
          selection.branch,
        );
      }
      break;
    }
    case "gcpCloudSql": {
      const credential = gcpCredential(integration);
      const oidcToken = requiredOidcToken(input.oidcToken);
      if (kind === "projects") return listGcpProjects(credential);
      if (kind === "instances" && selection.project === credential.projectId) {
        return listGcpCloudSqlInstances(credential, oidcToken);
      }
      if (
        kind === "databases"
        && selection.project === credential.projectId
        && isSegment(selection.instance)
      ) {
        const engine = selection.engine === "postgres" || selection.engine === "mysql"
          ? selection.engine
          : null;
        return listGcpCloudSqlDatabases(
          credential,
          oidcToken,
          selection.instance,
          engine,
        );
      }
      break;
    }
    default:
      throw new Error("Managed credential provider is not available");
  }
  throw new ProviderRequestError(
    integration.provider,
    "Invalid provider resource query",
    400,
  );
}

export async function validateManagedProviderResource(input: {
  integration: ActiveProviderIntegration;
  resource: ManagedProviderResource;
  oidcToken?: string | null;
}) {
  switch (input.integration.provider) {
    case "planetScale":
      return validatePlanetScaleResource(
        await providerAccessToken(input.integration),
        input.resource as PlanetScaleResource,
      );
    case "neon":
      return validateNeonResource(
        neonCredential(input.integration),
        input.resource as NeonResource,
      );
    case "gcpCloudSql":
      return validateGcpCloudSqlResource(
        gcpCredential(input.integration),
        requiredOidcToken(input.oidcToken),
        input.resource as GcpCloudSqlResource,
      );
    default:
      throw new Error("Managed credential provider is not available");
  }
}

async function bestEffortRevokeLease(input: {
  integration: ActiveProviderIntegration;
  resource: ManagedProviderResource;
  lease: ManagedProviderLease;
  planetScaleToken?: string;
}) {
  if (
    input.integration.provider === "planetScale"
    && (input.lease.externalCredentialKind === "role"
      || input.lease.externalCredentialKind === "password")
  ) {
    const token = input.planetScaleToken
      ?? await providerAccessToken(input.integration);
    await revokePlanetScaleLease(
      token,
      input.resource as PlanetScaleResource,
      input.lease.externalCredentialKind,
      input.lease.externalCredentialId,
    );
  } else if (
    input.integration.provider === "neon"
    && input.lease.externalCredentialKind === "role"
  ) {
    await revokeNeonLease(
      neonCredential(input.integration),
      input.resource as NeonResource,
      input.lease.externalCredentialId,
    );
  }
  // Cloud SQL IAM access tokens have no token-revocation API. If the one-time
  // response was not delivered, it is unreachable and expires within 15 minutes.
}

export async function issueManagedLease(input: {
  organizationId: string;
  connectionId: string;
  userId: string;
  memberId: string;
  role: WorkspaceRoleName;
  connectionRevision: number;
  engine: "postgres" | "mysql";
  accessMode: "read" | "write";
  integration: ActiveProviderIntegration;
  resource: ManagedProviderResource;
  oidcToken?: string | null;
}): Promise<ManagedProviderLease & { leaseId: string }> {
  const leaseId = crypto.randomUUID();
  const label = `dopedb-${input.userId.replace(/-/g, "").slice(0, 8)}-${
    leaseId.replace(/-/g, "").slice(0, 8)
  }`;
  const authority: ManagedLeaseAuthority = {
    leaseId,
    organizationId: input.organizationId,
    memberId: input.memberId,
    userId: input.userId,
    role: input.role,
    connectionId: input.connectionId,
    integrationId: input.integration.id,
    provider: input.integration.provider,
    connectionRevision: input.connectionRevision,
    engine: input.engine,
    accessMode: input.accessMode,
  };
  const reservation = await reserveManagedLeaseIfUnblocked(authority);
  if (reservation !== "reserved") {
    throw new ProviderRequestError(
      input.integration.provider,
      reservation === "limit"
        ? "Too many active database sessions. Retry after leases expire."
        : "Workspace database authority is changing. Retry shortly.",
      reservation === "limit" ? 429 : 409,
    );
  }

  let planetScaleToken: string | undefined;
  let lease: ManagedProviderLease;
  try {
    if (input.integration.provider === "neon") {
      // Sweep a small bounded batch synchronously so a delayed scheduler cannot allow
      // dormant roles to grow monotonically without adding long lease-request latency.
      const cleanup = await cleanupExpiredManagedLeases({
        integrationId: input.integration.id,
        limit: 2,
      });
      if (cleanup.deferred > 0) {
        throw new ProviderRequestError(
          "neon",
          "Expired Neon database access could not be cleaned up",
          503,
        );
      }
    }
    switch (input.integration.provider) {
      case "planetScale":
        planetScaleToken = await providerAccessToken(input.integration);
        lease = await issuePlanetScaleLease(
          planetScaleToken,
          input.resource as PlanetScaleResource,
          input.accessMode,
          label,
        );
        break;
      case "neon":
        lease = await issueNeonLease({
          credential: neonCredential(input.integration),
          resource: input.resource as NeonResource,
          accessMode: input.accessMode,
          role: neonRoleForLease(input.userId, leaseId),
        });
        break;
      case "gcpCloudSql":
        lease = await issueGcpCloudSqlLease({
          credential: gcpCredential(input.integration),
          oidcToken: requiredOidcToken(input.oidcToken),
          resource: input.resource as GcpCloudSqlResource,
          accessMode: input.accessMode,
          externalCredentialId: leaseId,
        });
        break;
      default:
        throw new Error("Managed credential provider is not available");
    }
  } catch (error) {
    await db.update(workspaceCredentialLease)
      .set(input.integration.provider === "neon"
        ? { expiresAt: new Date() }
        : { revokedAt: new Date() })
      .where(eq(workspaceCredentialLease.id, leaseId))
      .catch(() => undefined);
    throw error;
  }

  try {
    if (!await finalizeManagedLeaseIfUnblocked(authority, lease)) {
      throw new Error("Managed lease reservation is no longer active");
    }
  } catch (error) {
    let revoked = false;
    try {
      await bestEffortRevokeLease({
        integration: input.integration,
        resource: input.resource,
        lease,
        planetScaleToken,
      });
      revoked = true;
    } catch {
      // Leave failed Neon cleanup visible to the durable expiry sweeper.
    }
    await db.update(workspaceCredentialLease)
      .set(input.integration.provider === "neon" && !revoked
        ? { expiresAt: new Date() }
        : { revokedAt: new Date() })
      .where(eq(workspaceCredentialLease.id, leaseId))
      .catch(() => undefined);
    throw error;
  }
  return { ...lease, leaseId };
}

type LeaseCleanupRow = {
  id: string;
  organizationId: string;
  connectionOrganizationId: string;
  connectionIntegrationId: string | null;
  integrationId: string;
  userId: string;
  provider: string;
  credentialId: string;
  credentialKind: string;
  expiresAt: Date;
  providerResource: unknown;
  cleanupClaim?: {
    attempt: number;
  };
};

export function managedLeaseAuthorityMatches(input: {
  leaseOrganizationId: string;
  connectionOrganizationId: string;
  leaseIntegrationId: string;
  connectionIntegrationId: string | null;
  integrationOrganizationId: string;
  leaseProvider: string;
  integrationProvider: string;
}) {
  return input.connectionOrganizationId === input.leaseOrganizationId
    && input.connectionIntegrationId === input.leaseIntegrationId
    && input.integrationOrganizationId === input.leaseOrganizationId
    && input.integrationProvider === input.leaseProvider;
}

async function markLeaseRevoked(
  id: string,
  cleanupClaim?: LeaseCleanupRow["cleanupClaim"],
) {
  const predicates = [
    eq(workspaceCredentialLease.id, id),
    isNull(workspaceCredentialLease.revokedAt),
  ];
  if (cleanupClaim) {
    predicates.push(
      eq(workspaceCredentialLease.cleanupAttempts, cleanupClaim.attempt),
      isNotNull(workspaceCredentialLease.cleanupClaimedAt),
    );
  }
  const rows = await db.update(workspaceCredentialLease)
    .set({
      revokedAt: new Date(),
      cleanupClaimedAt: null,
      cleanupNextAttemptAt: null,
    })
    .where(and(...predicates))
    .returning({ id: workspaceCredentialLease.id });
  return rows.length === 1;
}

async function scheduleLeaseCleanupRetry(lease: LeaseCleanupRow) {
  const cleanupClaim = lease.cleanupClaim;
  if (!cleanupClaim) return false;
  const rows = await db.update(workspaceCredentialLease)
    .set({
      cleanupClaimedAt: null,
      cleanupNextAttemptAt: new Date(
        Date.now() + managedLeaseCleanupRetryDelayMs(cleanupClaim.attempt),
      ),
    })
    .where(and(
      eq(workspaceCredentialLease.id, lease.id),
      eq(workspaceCredentialLease.cleanupAttempts, cleanupClaim.attempt),
      isNotNull(workspaceCredentialLease.cleanupClaimedAt),
      isNull(workspaceCredentialLease.revokedAt),
    ))
    .returning({ id: workspaceCredentialLease.id });
  return rows.length === 1;
}

async function revokeLeaseRows(
  leases: LeaseCleanupRow[],
): Promise<LeaseRevocationResult> {
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
  const now = Date.now();
  let revoked = 0;
  let deferred = 0;

  for (const lease of leases) {
    const integration = integrationMap.get(lease.integrationId);
    const expired = lease.expiresAt.valueOf() <= now;
    try {
      if (
        !integration
        || !managedLeaseAuthorityMatches({
          leaseOrganizationId: lease.organizationId,
          connectionOrganizationId: lease.connectionOrganizationId,
          leaseIntegrationId: lease.integrationId,
          connectionIntegrationId: lease.connectionIntegrationId,
          integrationOrganizationId: integration.organizationId,
          leaseProvider: lease.provider,
          integrationProvider: integration.provider,
        })
      ) {
        throw new Error("Lease database authority is inconsistent");
      }
      if (integration.provider === "gcpCloudSql") {
        // IAM login tokens have no revocation API. Once expired they are safe to
        // retire from the audit index; live tokens remain an explicit deferral.
        if (!expired) {
          deferred += 1;
          continue;
        }
      } else if (lease.credentialKind === "pending") {
        if (!expired) {
          deferred += 1;
          continue;
        }
        if (integration.provider === "neon") {
          const resource = parseManagedProviderResource(
            integration.provider,
            lease.providerResource,
          );
          await revokeNeonLease(
            neonCredential(integration),
            resource as NeonResource,
            neonRoleForLease(lease.userId, lease.id),
          );
        }
        // Other pending records never persisted an external credential identifier.
      } else {
        const resource = parseManagedProviderResource(
          integration.provider,
          lease.providerResource,
        );
        if (
          integration.provider === "planetScale"
          && (lease.credentialKind === "role" || lease.credentialKind === "password")
        ) {
          await revokePlanetScaleLease(
            await providerAccessToken(integration),
            resource as PlanetScaleResource,
            lease.credentialKind,
            lease.credentialId,
          );
        } else if (
          integration.provider === "neon"
          && lease.credentialKind === "role"
        ) {
          await revokeNeonLease(
            neonCredential(integration),
            resource as NeonResource,
            lease.credentialId,
          );
        } else if (integration.provider !== "gcpCloudSql") {
          throw new Error("Lease provider is unavailable");
        }
      }
      if (await markLeaseRevoked(lease.id, lease.cleanupClaim)) revoked += 1;
    } catch (error) {
      if (error instanceof ProviderRequestError && error.status === 404) {
        if (await markLeaseRevoked(lease.id, lease.cleanupClaim)) revoked += 1;
        continue;
      }
      if (!lease.cleanupClaim || await scheduleLeaseCleanupRetry(lease)) {
        deferred += 1;
      }
    }
  }
  return { revoked, deferred };
}

export async function revokeActiveLeases(
  filter: LeaseRevocationFilter,
): Promise<LeaseRevocationResult> {
  const predicates = [
    eq(workspaceCredentialLease.organizationId, filter.organizationId),
    isNull(workspaceCredentialLease.revokedAt),
  ];
  if (filter.leaseId) {
    predicates.push(eq(workspaceCredentialLease.id, filter.leaseId));
  }
  if (filter.userId) predicates.push(eq(workspaceCredentialLease.userId, filter.userId));
  if (filter.connectionId) {
    predicates.push(eq(workspaceCredentialLease.connectionId, filter.connectionId));
  }
  if (filter.integrationId) {
    predicates.push(eq(workspaceCredentialLease.integrationId, filter.integrationId));
  }
  const leases = await db.select({
    id: workspaceCredentialLease.id,
    organizationId: workspaceCredentialLease.organizationId,
    connectionOrganizationId: workspaceConnection.organizationId,
    connectionIntegrationId: workspaceConnection.providerIntegrationId,
    integrationId: workspaceCredentialLease.integrationId,
    userId: workspaceCredentialLease.userId,
    provider: workspaceCredentialLease.provider,
    credentialId: workspaceCredentialLease.externalCredentialId,
    credentialKind: workspaceCredentialLease.externalCredentialKind,
    expiresAt: workspaceCredentialLease.expiresAt,
    providerResource: workspaceConnection.providerResource,
  }).from(workspaceCredentialLease)
    .innerJoin(
      workspaceConnection,
      eq(workspaceCredentialLease.connectionId, workspaceConnection.id),
    )
    .where(and(...predicates))
    .orderBy(asc(workspaceCredentialLease.expiresAt));
  return revokeLeaseRows(leases);
}

type ClaimedLeaseRow = {
  id: string;
  organizationId: string;
  connectionOrganizationId: string;
  connectionIntegrationId: string | null;
  integrationId: string;
  userId: string;
  provider: string;
  credentialId: string;
  credentialKind: string;
  expiresAt: Date | string;
  providerResource: unknown;
  cleanupAttempt: number | string;
};

async function claimExpiredManagedLeases(input: {
  integrationId?: string;
  limit: number;
}): Promise<LeaseCleanupRow[]> {
  const rankedIntegrationFilter = input.integrationId
    ? sql`AND ranked_lease."integration_id" = ${input.integrationId}::uuid`
    : sql``;
  const candidateIntegrationFilter = input.integrationId
    ? sql`AND lease."integration_id" = ${input.integrationId}::uuid`
    : sql``;
  const result = await db.execute<ClaimedLeaseRow>(sql`
    WITH ranked AS (
      SELECT ranked_lease."id",
             ranked_lease."cleanup_attempts",
             COALESCE(
               ranked_lease."cleanup_next_attempt_at",
               ranked_lease."expires_at"
             ) AS ready_at,
             ROW_NUMBER() OVER (
               PARTITION BY ranked_lease."organization_id"
               ORDER BY ranked_lease."cleanup_attempts" ASC,
                        COALESCE(
                          ranked_lease."cleanup_next_attempt_at",
                          ranked_lease."expires_at"
                        ) ASC,
                        ranked_lease."expires_at" ASC,
                        ranked_lease."id" ASC
             ) AS tenant_rank
      FROM ${workspaceCredentialLease} AS ranked_lease
      INNER JOIN ${workspaceConnection} AS ranked_connection
        ON ranked_connection."id" = ranked_lease."connection_id"
      WHERE ranked_lease."revoked_at" IS NULL
        AND ranked_lease."expires_at" <= CURRENT_TIMESTAMP
        AND (
          ranked_lease."cleanup_next_attempt_at" IS NULL
          OR ranked_lease."cleanup_next_attempt_at" <= CURRENT_TIMESTAMP
        )
        AND (
          ranked_lease."cleanup_claimed_at" IS NULL
          OR ranked_lease."cleanup_claimed_at"
            < CURRENT_TIMESTAMP
              - (${CLEANUP_CLAIM_STALE_SECONDS} * INTERVAL '1 second')
        )
        ${rankedIntegrationFilter}
    ),
    candidates AS (
      SELECT lease."id"
      FROM ${workspaceCredentialLease} AS lease
      INNER JOIN ranked ON ranked."id" = lease."id"
      WHERE lease."revoked_at" IS NULL
        AND lease."expires_at" <= CURRENT_TIMESTAMP
        AND (
          lease."cleanup_next_attempt_at" IS NULL
          OR lease."cleanup_next_attempt_at" <= CURRENT_TIMESTAMP
        )
        AND (
          lease."cleanup_claimed_at" IS NULL
          OR lease."cleanup_claimed_at"
            < CURRENT_TIMESTAMP
              - (${CLEANUP_CLAIM_STALE_SECONDS} * INTERVAL '1 second')
        )
        ${candidateIntegrationFilter}
      ORDER BY ranked."cleanup_attempts" ASC,
               ranked.tenant_rank ASC,
               ranked.ready_at ASC,
               lease."id" ASC
      FOR UPDATE OF lease SKIP LOCKED
      LIMIT ${input.limit}
    ),
    claimed AS (
      UPDATE ${workspaceCredentialLease} AS lease
      SET "cleanup_claimed_at" = CURRENT_TIMESTAMP,
          "cleanup_attempts" = lease."cleanup_attempts" + 1
      FROM candidates
      WHERE lease."id" = candidates."id"
      RETURNING lease."id",
                lease."organization_id",
                lease."integration_id",
                lease."user_id",
                lease."provider",
                lease."external_credential_id",
                lease."external_credential_kind",
                lease."expires_at",
                lease."connection_id",
                lease."cleanup_attempts"
    )
    SELECT claimed."id" AS "id",
           claimed."organization_id" AS "organizationId",
           connection."organization_id" AS "connectionOrganizationId",
           connection."provider_integration_id"::text AS "connectionIntegrationId",
           claimed."integration_id" AS "integrationId",
           claimed."user_id" AS "userId",
           claimed."provider" AS "provider",
           claimed."external_credential_id" AS "credentialId",
           claimed."external_credential_kind" AS "credentialKind",
           claimed."expires_at" AS "expiresAt",
           connection."provider_resource" AS "providerResource",
           claimed."cleanup_attempts" AS "cleanupAttempt"
    FROM claimed
    INNER JOIN ${workspaceConnection} AS connection
      ON connection."id" = claimed."connection_id"
    INNER JOIN ranked ON ranked."id" = claimed."id"
    ORDER BY ranked."cleanup_attempts" ASC,
             ranked.tenant_rank ASC,
             ranked.ready_at ASC,
             claimed."id" ASC
  `);
  return result.rows.map((row) => {
    const expiresAt = row.expiresAt instanceof Date
      ? row.expiresAt
      : new Date(row.expiresAt);
    const cleanupAttempt = Number(row.cleanupAttempt);
    if (
      Number.isNaN(expiresAt.valueOf())
      || !Number.isSafeInteger(cleanupAttempt)
      || cleanupAttempt < 1
    ) {
      throw new Error("Invalid managed lease cleanup claim");
    }
    return {
      id: row.id,
      organizationId: row.organizationId,
      connectionOrganizationId: row.connectionOrganizationId,
      connectionIntegrationId: row.connectionIntegrationId,
      integrationId: row.integrationId,
      userId: row.userId,
      provider: row.provider,
      credentialId: row.credentialId,
      credentialKind: row.credentialKind,
      expiresAt,
      providerResource: row.providerResource,
      cleanupClaim: { attempt: cleanupAttempt },
    };
  });
}

export async function cleanupExpiredManagedLeases(input: {
  integrationId?: string;
  limit?: number;
} = {}): Promise<ExpiredLeaseCleanupResult> {
  const limit = input.limit ?? 20;
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) {
    throw new Error("Invalid managed lease cleanup limit");
  }
  const leases = await claimExpiredManagedLeases({
    integrationId: input.integrationId,
    limit,
  });
  return {
    scanned: leases.length,
    ...await revokeLeaseRows(leases),
  };
}
