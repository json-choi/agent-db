import type { SQL } from "drizzle-orm";
import { PgDialect } from "drizzle-orm/pg-core";
import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  authorizationRows,
  authorizeWorkspaceMock,
  batchMock,
  claimMock,
  clearMock,
  connectionFindMock,
  releaseMock,
  revokeMock,
  updateSetMock,
} = vi.hoisted(() => {
  const updateReturningMock = vi.fn(() => ({ kind: "connection-update" }));
  const updateWhereMock = vi.fn(() => ({ returning: updateReturningMock }));
  return {
    authorizationRows: [] as unknown[],
    authorizeWorkspaceMock: vi.fn(),
    batchMock: vi.fn(),
    claimMock: vi.fn(),
    clearMock: vi.fn(),
    connectionFindMock: vi.fn(),
    releaseMock: vi.fn(),
    revokeMock: vi.fn(),
    updateSetMock: vi.fn(() => ({ where: updateWhereMock })),
  };
});

vi.mock("server-only", () => ({}));
vi.mock("../../../../../../../lib/db", () => ({
  db: {
    batch: batchMock,
    execute: vi.fn((query: unknown) => query),
    query: { workspaceConnection: { findFirst: connectionFindMock } },
    select: vi.fn(() => {
      const builder = {
        from: vi.fn(),
        leftJoin: vi.fn(),
        where: vi.fn(),
        limit: vi.fn(async () => authorizationRows),
      };
      builder.from.mockReturnValue(builder);
      builder.leftJoin.mockReturnValue(builder);
      builder.where.mockReturnValue(builder);
      return builder;
    }),
    update: vi.fn(() => ({ set: updateSetMock })),
  },
}));
vi.mock("../../../../../../../lib/env", () => ({
  env: { appOrigin: () => "https://app.example" },
}));
vi.mock("../../../../../../../lib/provider-integrations", () => ({
  revokeActiveLeases: revokeMock,
}));
vi.mock("../../../../../../../lib/revocation-gates", () => ({
  claimRevocationGate: claimMock,
  clearRevocationGate: clearMock,
  releaseRevocationGateClaim: releaseMock,
}));
vi.mock("../../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { DELETE, PATCH, POST } from "./route";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const connectionId = "22222222-2222-4222-8222-222222222222";
const context = { params: Promise.resolve({ workspaceId, connectionId }) };
const claim = {
  kind: "connection",
  organizationId: workspaceId,
  connectionId,
  claimId: "33333333-3333-4333-8333-333333333333",
  claimedAt: new Date("2026-07-23T00:00:00Z"),
  pendingAt: new Date("2026-07-23T00:00:00Z"),
  firstPending: true,
  connectionRevision: 2,
};

function request(method: "PATCH" | "DELETE") {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/connections/${connectionId}`,
    {
      method,
      headers: {
        "content-type": "application/json",
        origin: "https://app.example",
      },
      ...(method === "PATCH"
        ? {
            body: JSON.stringify({
              name: "Production",
              engine: "postgres",
              provider: "generic",
              host: "db.example.com",
              port: 5432,
              database: "app",
              sslmode: "verify-full",
              readonlyDefault: true,
              allowWrites: false,
            }),
          }
        : {}),
    },
  );
}

function authorizationRequest(action: "read" | "write") {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/connections/${connectionId}`,
    {
      method: "POST",
      headers: {
        "content-type": "application/json",
        origin: "https://app.example",
      },
      body: JSON.stringify({ action }),
    },
  );
}

const connection = {
  id: connectionId,
  name: "Production",
  engine: "postgres",
  provider: "generic",
  driverId: null,
  host: "db.example.com",
  port: 5432,
  databaseName: "app",
  sslmode: "verify-full",
  readonlyDefault: true,
  allowWrites: true,
  environment: null,
  schemaGroup: null,
  credentialMode: "member_local",
  providerIntegrationId: null,
  revision: 1,
  updatedAt: new Date("2026-07-23T00:00:00Z"),
  revocationPendingAt: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  authorizationRows.splice(0, authorizationRows.length, {
    id: connectionId,
    revision: connection.revision,
    allowWrites: connection.allowWrites,
    credentialMode: connection.credentialMode,
    provider: connection.provider,
    providerIntegrationId: connection.providerIntegrationId,
    revocationPendingAt: connection.revocationPendingAt,
    integrationStatus: null,
    integrationProvider: null,
    integrationRevokedAt: null,
    integrationRevocationPendingAt: null,
    integrationRevocationClaimId: null,
  });
  authorizeWorkspaceMock.mockResolvedValue({
    ok: true,
    role: "admin",
    accessMode: "manage",
    session: { user: { id: "admin-user" } },
  });
  connectionFindMock.mockResolvedValue(connection);
  claimMock.mockResolvedValue(claim);
  clearMock.mockResolvedValue(true);
  releaseMock.mockResolvedValue(true);
  revokeMock.mockResolvedValue({ revoked: 0, deferred: 0 });
  batchMock.mockResolvedValue([
    [{ ...connection, revision: 3, updatedAt: new Date() }],
    {},
  ]);
});

describe("connection authority mutation gate", () => {
  it("fails closed while a connection authority mutation is pending", async () => {
    authorizationRows[0] = {
      ...authorizationRows[0] as object,
      revocationPendingAt: new Date(),
    };

    const response = await POST(authorizationRequest("read"), context);

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: "Connection access is changing. Retry shortly.",
    });
  });

  it("rejects write authorization when the shared template disables writes", async () => {
    authorizationRows[0] = {
      ...authorizationRows[0] as object,
      allowWrites: false,
    };

    const response = await POST(authorizationRequest("write"), context);

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({
      error: "Writing is disabled for this connection",
    });
  });

  it.each([
    {
      label: "pending",
      integrationStatus: "active",
      integrationProvider: "neon",
      integrationRevokedAt: null,
      integrationRevocationPendingAt: new Date(),
      integrationRevocationClaimId: "44444444-4444-4444-8444-444444444444",
    },
    {
      label: "revoked",
      integrationStatus: "revoked",
      integrationProvider: "neon",
      integrationRevokedAt: new Date(),
      integrationRevocationPendingAt: null,
      integrationRevocationClaimId: null,
    },
    {
      label: "inactive",
      integrationStatus: "inactive",
      integrationProvider: "neon",
      integrationRevokedAt: null,
      integrationRevocationPendingAt: null,
      integrationRevocationClaimId: null,
    },
    {
      label: "provider mismatch",
      integrationStatus: "active",
      integrationProvider: "gcpCloudSql",
      integrationRevokedAt: null,
      integrationRevocationPendingAt: null,
      integrationRevocationClaimId: null,
    },
  ])("fails managed authorization closed for a $label integration", async (state) => {
    authorizationRows[0] = {
      ...authorizationRows[0] as object,
      credentialMode: "managed",
      provider: "neon",
      providerIntegrationId: "33333333-3333-4333-8333-333333333333",
      ...state,
    };

    const response = await POST(authorizationRequest("read"), context);

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: "Managed provider access is unavailable or changing",
    });
  });

  it("authorizes a managed connection only with its active matching integration", async () => {
    authorizationRows[0] = {
      ...authorizationRows[0] as object,
      credentialMode: "managed",
      provider: "neon",
      providerIntegrationId: "33333333-3333-4333-8333-333333333333",
      integrationStatus: "active",
      integrationProvider: "neon",
      integrationRevokedAt: null,
      integrationRevocationPendingAt: null,
      integrationRevocationClaimId: null,
    };

    const response = await POST(authorizationRequest("read"), context);

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      allowed: true,
      action: "read",
      revision: 1,
    });
  });

  it("rejects a concurrent PATCH claimant before lease revocation", async () => {
    claimMock.mockResolvedValue(null);

    const response = await PATCH(request("PATCH"), context);

    expect(response.status).toBe(409);
    expect(revokeMock).not.toHaveBeenCalled();
    expect(batchMock).not.toHaveBeenCalled();
  });

  it("rejects a claim that does not follow the parsed template revision", async () => {
    claimMock.mockResolvedValue({ ...claim, connectionRevision: 4 });

    const response = await PATCH(request("PATCH"), context);

    expect(response.status).toBe(409);
    expect(clearMock).toHaveBeenCalledWith(expect.objectContaining({
      connectionRevision: 4,
    }));
    expect(releaseMock).not.toHaveBeenCalled();
    expect(revokeMock).not.toHaveBeenCalled();
    expect(batchMock).not.toHaveBeenCalled();
  });

  it("releases only a stale takeover claim on a revision mismatch", async () => {
    claimMock.mockResolvedValue({
      ...claim,
      firstPending: false,
      connectionRevision: 4,
    });

    const response = await PATCH(request("PATCH"), context);

    expect(response.status).toBe(409);
    expect(releaseMock).toHaveBeenCalledWith(expect.objectContaining({
      firstPending: false,
      connectionRevision: 4,
    }));
    expect(clearMock).not.toHaveBeenCalled();
    expect(revokeMock).not.toHaveBeenCalled();
    expect(batchMock).not.toHaveBeenCalled();
  });

  it("increments revision again when the template mutation commits", async () => {
    const response = await PATCH(request("PATCH"), context);

    expect(response.status).toBe(200);
    const values = updateSetMock.mock.calls.at(0)?.at(0) as
      | { revision?: SQL }
      | undefined;
    expect(values?.revision).toBeDefined();
    const compiled = new PgDialect().sqlToQuery(values!.revision!);
    expect(compiled.sql.replace(/\s+/g, " ")).toContain(
      "\"workspace_connection\".\"revision\" + 1",
    );
  });

  it("keeps DELETE pending and releases only its claim when revocation defers", async () => {
    revokeMock.mockResolvedValue({ revoked: 0, deferred: 1 });

    const response = await DELETE(request("DELETE"), context);

    expect(response.status).toBe(409);
    expect(revokeMock).toHaveBeenCalledWith({
      organizationId: workspaceId,
      connectionId,
    });
    expect(releaseMock).toHaveBeenCalledWith(claim);
    expect(batchMock).not.toHaveBeenCalled();
  });

  it("releases the claim when revocation throws and never mutates authority", async () => {
    revokeMock.mockRejectedValue(new Error("provider unavailable"));

    await expect(PATCH(request("PATCH"), context)).rejects.toThrow(
      "provider unavailable",
    );
    expect(releaseMock).toHaveBeenCalledWith(claim);
    expect(batchMock).not.toHaveBeenCalled();
  });
});
