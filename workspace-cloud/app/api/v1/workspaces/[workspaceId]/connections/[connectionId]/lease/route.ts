// Native-client-only one-time credential issuance. The provider secret is returned
// over HTTPS exactly once and is absent from all database and audit writes.
import { and, count, eq, gt, isNull, sql } from "drizzle-orm";
import { db } from "../../../../../../../../lib/db";
import { isUuid, jsonError, privateJson } from "../../../../../../../../lib/http";
import {
  activeProviderIntegration,
  issueManagedLease,
  parseManagedProviderResource,
  revokeActiveLeases,
} from "../../../../../../../../lib/provider-integrations";
import { vercelOidcToken } from "../../../../../../../../lib/providers/gcp-cloud-sql";
import { ProviderRequestError } from "../../../../../../../../lib/providers/provider-types";
import { managedLeaseStillDeliverable } from "../../../../../../../../lib/revocation-gates";
import {
  workspaceAuditEvent,
  workspaceConnection,
  workspaceCredentialLease,
  rateLimit,
} from "../../../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../../../lib/workspace-authorization";

type RouteContext = {
  params: Promise<{ workspaceId: string; connectionId: string }>;
};

async function consumeLeaseBudget(organizationId: string, userId: string) {
  const now = Date.now();
  const windowStart = now - 60_000;
  const key = `workspace-lease:${organizationId}:${userId}`;
  const result = await db.execute<{ value: number }>(sql`
    INSERT INTO ${rateLimit} ("id", "key", "count", "last_request")
    VALUES (${crypto.randomUUID()}, ${key}, 1, ${now})
    ON CONFLICT ("key") DO UPDATE SET
      "count" = CASE
        WHEN ${rateLimit.lastRequest} < ${windowStart} THEN 1
        ELSE ${rateLimit.count} + 1
      END,
      "last_request" = ${now}
    RETURNING "count" AS "value"
  `);
  return Number(result.rows[0]?.value ?? Number.POSITIVE_INFINITY) <= 5;
}

export async function POST(request: Request, context: RouteContext) {
  if (!request.headers.get("authorization")?.startsWith("Bearer ")) {
    return jsonError("Desktop bearer authentication is required", 401);
  }
  const { workspaceId, connectionId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "read");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const connection = await db.query.workspaceConnection.findFirst({
    where: and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ),
    columns: {
      id: true,
      engine: true,
      allowWrites: true,
      credentialMode: true,
      providerIntegrationId: true,
      providerResource: true,
      revision: true,
    },
  });
  if (
    !connection
    || connection.credentialMode !== "managed"
    || !connection.providerIntegrationId
  ) {
    return jsonError("Managed database access is not available", 409);
  }
  const integration = await activeProviderIntegration(
    workspaceId,
    connection.providerIntegrationId,
  );
  if (!integration) return jsonError("Provider integration not found", 404);
  let resource;
  try {
    resource = parseManagedProviderResource(
      integration.provider,
      connection.providerResource,
    );
  } catch {
    return jsonError("Managed database resource is invalid", 409);
  }
  if (resource.engine !== connection.engine) {
    return jsonError("Managed database engine does not match the connection", 409);
  }
  const [activeCount] = await db.select({ value: count() })
    .from(workspaceCredentialLease)
    .where(and(
      eq(workspaceCredentialLease.organizationId, workspaceId),
      eq(workspaceCredentialLease.connectionId, connectionId),
      eq(workspaceCredentialLease.userId, authorization.session.user.id),
      isNull(workspaceCredentialLease.revokedAt),
      gt(workspaceCredentialLease.expiresAt, new Date()),
    ));
  if (activeCount.value >= 5) {
    return jsonError("Too many active database sessions. Retry after leases expire.", 429);
  }
  if (!await consumeLeaseBudget(workspaceId, authorization.session.user.id)) {
    return jsonError("Managed database access is being opened too quickly. Retry shortly.", 429);
  }
  const accessMode = connection.allowWrites
    && (authorization.accessMode === "write" || authorization.accessMode === "manage")
    ? "write"
    : "read";
  try {
    const lease = await issueManagedLease({
      organizationId: workspaceId,
      connectionId,
      userId: authorization.session.user.id,
      memberId: authorization.membership.id,
      role: authorization.role,
      connectionRevision: connection.revision,
      engine: resource.engine,
      accessMode,
      integration,
      resource,
      oidcToken: vercelOidcToken(request),
    });
    try {
      await db.insert(workspaceAuditEvent).values({
        organizationId: workspaceId,
        actorUserId: authorization.session.user.id,
        action: "credential.lease.issue",
        resourceType: "connection",
        resourceId: connectionId,
        redactedSummary: {
          provider: integration.provider,
          accessMode,
          expiresAt: lease.expiresAt,
        },
        requestId: crypto.randomUUID(),
      });
    } catch {
      await revokeActiveLeases({
        organizationId: workspaceId,
        leaseId: lease.leaseId,
        userId: authorization.session.user.id,
        connectionId,
      });
      return jsonError("Database access could not be audited", 500);
    }
    const [currentAuthorization, deliverable] = await Promise.all([
      authorizeWorkspace(
        request,
        workspaceId,
        accessMode === "write" ? "write" : "read",
      ),
      managedLeaseStillDeliverable({
        leaseId: lease.leaseId,
        organizationId: workspaceId,
        memberId: authorization.membership.id,
        userId: authorization.session.user.id,
        role: authorization.role,
        connectionId,
        connectionRevision: connection.revision,
        engine: resource.engine,
        integrationId: integration.id,
        provider: integration.provider,
        accessMode,
      }, lease),
    ]);
    if (
      !currentAuthorization.ok
      || currentAuthorization.role !== authorization.role
      || !deliverable
    ) {
      await revokeActiveLeases({
        organizationId: workspaceId,
        leaseId: lease.leaseId,
        userId: authorization.session.user.id,
        connectionId,
      });
      return jsonError("Workspace database authority changed. Retry with current access.", 409);
    }
    return privateJson({
      lease: {
        id: lease.leaseId,
        provider: integration.provider,
        engine: resource.engine,
        host: lease.host,
        port: lease.port,
        database: lease.database,
        username: lease.username,
        password: lease.password,
        sslmode: lease.sslmode,
        ...(lease.tlsServerCaPem
          ? { tlsServerCaPem: lease.tlsServerCaPem }
          : {}),
        accessMode,
        expiresAt: lease.expiresAt,
      },
    }, {
      headers: {
        pragma: "no-cache",
        expires: "0",
        "x-content-type-options": "nosniff",
      },
    });
  } catch (error) {
    if (error instanceof ProviderRequestError) {
      return jsonError(error.message, error.status);
    }
    return jsonError("Managed database access could not be issued", 502);
  }
}
