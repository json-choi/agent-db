import type { SQL } from "drizzle-orm";
import { PgDialect } from "drizzle-orm/pg-core";
import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  activeProviderIntegrationMock,
  auditValuesMock,
  authorizeWorkspaceMock,
  connectionFindFirstMock,
  dbExecuteMock,
  issueManagedLeaseMock,
  managedLeaseStillDeliverableMock,
  parseManagedProviderResourceMock,
  revokeActiveLeasesMock,
} = vi.hoisted(() => ({
  activeProviderIntegrationMock: vi.fn(),
  auditValuesMock: vi.fn(async () => undefined),
  authorizeWorkspaceMock: vi.fn(),
  connectionFindFirstMock: vi.fn(),
  dbExecuteMock: vi.fn(async () => ({ rows: [{ value: 1 }] })),
  issueManagedLeaseMock: vi.fn(),
  managedLeaseStillDeliverableMock: vi.fn(),
  parseManagedProviderResourceMock: vi.fn(),
  revokeActiveLeasesMock: vi.fn(),
}));

vi.mock("server-only", () => ({}));
vi.mock("../../../../../../../../lib/db", () => ({
  db: {
    execute: dbExecuteMock,
    insert: vi.fn(() => ({ values: auditValuesMock })),
    query: {
      workspaceConnection: { findFirst: connectionFindFirstMock },
    },
    select: vi.fn(() => ({
      from: vi.fn(() => ({
        where: vi.fn(async () => [{ value: 0 }]),
      })),
    })),
  },
}));
vi.mock("../../../../../../../../lib/provider-integrations", () => ({
  activeProviderIntegration: activeProviderIntegrationMock,
  issueManagedLease: issueManagedLeaseMock,
  parseManagedProviderResource: parseManagedProviderResourceMock,
  revokeActiveLeases: revokeActiveLeasesMock,
}));
vi.mock("../../../../../../../../lib/providers/gcp-cloud-sql", () => ({
  vercelOidcToken: vi.fn(() => "vercel-oidc"),
}));
vi.mock("../../../../../../../../lib/revocation-gates", () => ({
  managedLeaseStillDeliverable: managedLeaseStillDeliverableMock,
}));
vi.mock("../../../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { DELETE, POST } from "./route";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const connectionId = "22222222-2222-4222-8222-222222222222";
const integrationId = "33333333-3333-4333-8333-333333333333";
const memberId = "44444444-4444-4444-8444-444444444444";
const leaseId = "55555555-5555-4555-8555-555555555555";
const context = { params: Promise.resolve({ workspaceId, connectionId }) };
const integration = {
  id: integrationId,
  organizationId: workspaceId,
  provider: "neon",
  encryptedCredential: "sealed",
  credentialExpiresAt: null,
};
const resource = {
  engine: "postgres",
  project: "project-id",
  branch: "branch-id",
  database: "app",
};
const lease = {
  leaseId,
  externalCredentialId: "dopedb_role",
  externalCredentialKind: "role" as const,
  host: "db.example.test",
  port: 5432,
  database: "app",
  username: "dopedb_role",
  password: "one-time-secret",
  sslmode: "verify-full" as const,
  expiresAt: "2026-07-24T00:00:00.000Z",
};

function leaseRequest(accessMode?: "read" | "write") {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/connections/${connectionId}/lease`,
    {
      method: "POST",
      headers: {
        authorization: "Bearer desktop-session",
        ...(accessMode
          ? {
            "content-type": "application/json",
            "x-dopedb-managed-lease-contract": "access-v1",
          }
          : {}),
      },
      ...(accessMode ? { body: JSON.stringify({ accessMode }) } : {}),
    },
  );
}

function malformedLeaseRequest(payload: unknown) {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/connections/${connectionId}/lease`,
    {
      method: "POST",
      headers: {
        authorization: "Bearer desktop-session",
        "content-type": "application/json",
        "x-dopedb-managed-lease-contract": "access-v1",
      },
      body: JSON.stringify(payload),
    },
  );
}

function releaseRequest(id = leaseId) {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/connections/${connectionId}/lease`,
    {
      method: "DELETE",
      headers: {
        authorization: "Bearer desktop-session",
        "content-type": "application/json",
      },
      body: JSON.stringify({ leaseId: id }),
    },
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  authorizeWorkspaceMock.mockResolvedValue({
    ok: true,
    session: { user: { id: "member-user" } },
    membership: { id: memberId },
    role: "admin",
    accessMode: "manage",
  });
  connectionFindFirstMock.mockResolvedValue({
    id: connectionId,
    engine: "postgres",
    allowWrites: true,
    credentialMode: "managed",
    providerIntegrationId: integrationId,
    providerResource: resource,
    revision: 17,
  });
  activeProviderIntegrationMock.mockResolvedValue(integration);
  parseManagedProviderResourceMock.mockReturnValue(resource);
  issueManagedLeaseMock.mockResolvedValue(lease);
  managedLeaseStillDeliverableMock.mockResolvedValue(true);
  revokeActiveLeasesMock.mockResolvedValue({ revoked: 1, deferred: 0 });
});

describe("managed credential lease delivery", () => {
  it("passes the exact membership authority snapshot and returns a deliverable lease", async () => {
    const response = await POST(leaseRequest("write"), context);

    expect(response.status).toBe(200);
    expect(issueManagedLeaseMock).toHaveBeenCalledWith({
      organizationId: workspaceId,
      connectionId,
      userId: "member-user",
      memberId,
      role: "admin",
      connectionRevision: 17,
      engine: "postgres",
      accessMode: "write",
      integration,
      resource,
      oidcToken: "vercel-oidc",
    });
    expect(managedLeaseStillDeliverableMock).toHaveBeenCalledWith({
      leaseId,
      organizationId: workspaceId,
      memberId,
      userId: "member-user",
      role: "admin",
      connectionId,
      connectionRevision: 17,
      engine: "postgres",
      integrationId,
      provider: "neon",
      accessMode: "write",
    }, lease);
    await expect(response.json()).resolves.toMatchObject({
      lease: {
        id: leaseId,
        engine: "postgres",
        password: "one-time-secret",
        accessMode: "write",
      },
    });
    expect(revokeActiveLeasesMock).not.toHaveBeenCalled();
  });

  it("revokes only the issued lease and returns no secret when the final gate closes", async () => {
    managedLeaseStillDeliverableMock.mockResolvedValue(false);

    const response = await POST(leaseRequest("write"), context);
    const body = await response.json();

    expect(response.status).toBe(409);
    expect(body).toEqual({
      error: "Workspace database authority changed. Retry with current access.",
    });
    expect(JSON.stringify(body)).not.toContain("one-time-secret");
    expect(revokeActiveLeasesMock).toHaveBeenCalledOnce();
    expect(revokeActiveLeasesMock).toHaveBeenCalledWith({
      organizationId: workspaceId,
      leaseId,
      userId: "member-user",
      connectionId,
    });
  });

  it("withholds a GCP one-time IAM token when the fresh authority gate closes", async () => {
    const gcpIntegration = {
      ...integration,
      provider: "gcpCloudSql",
    };
    const gcpResource = {
      engine: "postgres",
      project: "sample-project",
      instance: "prod-db",
      database: "app",
      networkMode: "PUBLIC",
    };
    const gcpLease = {
      ...lease,
      externalCredentialId: leaseId,
      externalCredentialKind: "iamToken" as const,
      username: "dopedb-read@example.iam",
      password: "one-time-gcp-iam-token",
    };
    activeProviderIntegrationMock.mockResolvedValue(gcpIntegration);
    parseManagedProviderResourceMock.mockReturnValue(gcpResource);
    issueManagedLeaseMock.mockResolvedValue(gcpLease);
    managedLeaseStillDeliverableMock.mockResolvedValue(false);

    const response = await POST(leaseRequest("read"), context);
    const body = await response.json();

    expect(response.status).toBe(409);
    expect(JSON.stringify(body)).not.toContain("one-time-gcp-iam-token");
    expect(managedLeaseStillDeliverableMock).toHaveBeenCalledWith({
      leaseId,
      organizationId: workspaceId,
      memberId,
      userId: "member-user",
      role: "admin",
      connectionId,
      connectionRevision: 17,
      engine: "postgres",
      integrationId,
      provider: "gcpCloudSql",
      accessMode: "read",
    }, gcpLease);
    expect(revokeActiveLeasesMock).toHaveBeenCalledWith({
      organizationId: workspaceId,
      leaseId,
      userId: "member-user",
      connectionId,
    });
  });

  it("issues an explicitly requested read lease even for an administrator", async () => {
    const response = await POST(leaseRequest("read"), context);

    expect(response.status).toBe(200);
    expect(authorizeWorkspaceMock).toHaveBeenCalledWith(
      expect.any(Request),
      workspaceId,
      "read",
    );
    expect(issueManagedLeaseMock).toHaveBeenCalledWith(
      expect.objectContaining({ accessMode: "read" }),
    );
    await expect(response.json()).resolves.toMatchObject({
      lease: { accessMode: "read" },
    });
  });

  it("rejects write access when the shared connection is read-only", async () => {
    connectionFindFirstMock.mockResolvedValue({
      id: connectionId,
      engine: "postgres",
      allowWrites: false,
      credentialMode: "managed",
      providerIntegrationId: integrationId,
      providerResource: resource,
      revision: 17,
    });

    const response = await POST(leaseRequest("write"), context);

    expect(response.status).toBe(403);
    expect(issueManagedLeaseMock).not.toHaveBeenCalled();
    await expect(response.json()).resolves.toEqual({
      error: "Writing is disabled for this connection",
    });
  });

  it("requires legacy clients to upgrade instead of issuing over-privileged access", async () => {
    const response = await POST(leaseRequest(), context);

    expect(response.status).toBe(426);
    expect(issueManagedLeaseMock).not.toHaveBeenCalled();
    await expect(response.json()).resolves.toEqual({
      error: "Update DopeDB to use managed database access safely",
    });
  });

  it("rejects coercible non-string access modes", async () => {
    const response = await POST(
      malformedLeaseRequest({ accessMode: ["write"] }),
      context,
    );

    expect(response.status).toBe(400);
    expect(authorizeWorkspaceMock).not.toHaveBeenCalled();
    expect(issueManagedLeaseMock).not.toHaveBeenCalled();
    await expect(response.json()).resolves.toEqual({
      error: "Managed access mode must be read or write",
    });
  });

  it("releases only the authenticated member's exact managed lease", async () => {
    const response = await DELETE(releaseRequest(), context);

    expect(response.status).toBe(200);
    expect(authorizeWorkspaceMock).toHaveBeenCalledWith(
      expect.any(Request),
      workspaceId,
      "view",
    );
    expect(revokeActiveLeasesMock).toHaveBeenCalledWith({
      organizationId: workspaceId,
      leaseId,
      userId: "member-user",
      connectionId,
    });
    await expect(response.json()).resolves.toEqual({
      released: true,
      deferred: false,
    });
    expect(auditValuesMock).toHaveBeenCalledWith(expect.objectContaining({
      action: "credential.lease.release",
      redactedSummary: {
        released: 1,
        deferred: 0,
      },
    }));
  });

  it("rate-limits release requests before calling the provider revoker", async () => {
    dbExecuteMock.mockResolvedValueOnce({ rows: [{ value: 31 }] });

    const response = await DELETE(releaseRequest(), context);

    expect(response.status).toBe(429);
    expect(revokeActiveLeasesMock).not.toHaveBeenCalled();
    expect(auditValuesMock).not.toHaveBeenCalled();
    const calls = dbExecuteMock.mock.calls as unknown as Array<[SQL]>;
    const statement = calls[0]?.[0];
    if (!statement) throw new Error("Expected a release budget SQL statement");
    const query = new PgDialect().sqlToQuery(statement);
    expect(query.params).toContain(
      `workspace-lease-release:${workspaceId}:member-user`,
    );
  });

  it("does not grow the audit log for a no-op lease release", async () => {
    revokeActiveLeasesMock.mockResolvedValue({ revoked: 0, deferred: 0 });

    const response = await DELETE(releaseRequest(), context);

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({
      released: false,
      deferred: false,
    });
    expect(auditValuesMock).not.toHaveBeenCalled();
  });

  it("audits a deferred one-time credential release within the release budget", async () => {
    revokeActiveLeasesMock.mockResolvedValue({ revoked: 0, deferred: 1 });

    const response = await DELETE(releaseRequest(), context);

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({
      released: false,
      deferred: true,
    });
    expect(auditValuesMock).toHaveBeenCalledWith(expect.objectContaining({
      action: "credential.lease.release",
      redactedSummary: {
        released: 0,
        deferred: 1,
      },
    }));
  });
});
