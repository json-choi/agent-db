import { PgDialect } from "drizzle-orm/pg-core";
import type { SQL } from "drizzle-orm";
import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  activeProviderIntegrationMock,
  authorizeWorkspaceMock,
  batchMock,
  claimRevocationGateMock,
  connectionFindFirstMock,
  releaseRevocationGateClaimMock,
  revokeActiveLeasesMock,
  selectWhereMock,
  updateSetMock,
  updateWhereMock,
  validateManagedProviderResourceMock,
} = vi.hoisted(() => {
  const updateReturningMock = vi.fn(() => ({ kind: "connection-update" }));
  const updateWhere = vi.fn((_condition: unknown) => ({
    returning: updateReturningMock,
  }));
  const updateSet = vi.fn(() => ({ where: updateWhere }));
  return {
    activeProviderIntegrationMock: vi.fn(),
    authorizeWorkspaceMock: vi.fn(),
    batchMock: vi.fn(),
    claimRevocationGateMock: vi.fn(),
    connectionFindFirstMock: vi.fn(),
    releaseRevocationGateClaimMock: vi.fn(),
    revokeActiveLeasesMock: vi.fn(),
    selectWhereMock: vi.fn((condition: unknown) => ({
      getSQL: () => condition,
    })),
    updateSetMock: updateSet,
    updateWhereMock: updateWhere,
    validateManagedProviderResourceMock: vi.fn(),
  };
});

vi.mock("server-only", () => ({}));
vi.mock("../../../../../../../../lib/db", () => ({
  db: {
    batch: batchMock,
    execute: vi.fn((query: unknown) => query),
    query: {
      workspaceConnection: { findFirst: connectionFindFirstMock },
    },
    select: vi.fn(() => ({
      from: vi.fn(() => ({ where: selectWhereMock })),
    })),
    update: vi.fn(() => ({ set: updateSetMock })),
  },
}));
vi.mock("../../../../../../../../lib/env", () => ({
  env: { appOrigin: () => "https://app.example" },
}));
vi.mock("../../../../../../../../lib/provider-integrations", () => ({
  activeProviderIntegration: activeProviderIntegrationMock,
  parseManagedProviderResource: vi.fn(() => ({
    engine: "postgres",
    project: "project-id",
    branch: "branch-id",
    database: "app",
  })),
  revokeActiveLeases: revokeActiveLeasesMock,
  validateManagedProviderResource: validateManagedProviderResourceMock,
}));
vi.mock("../../../../../../../../lib/providers/gcp-cloud-sql", () => ({
  vercelOidcToken: vi.fn(() => "vercel-oidc"),
}));
vi.mock("../../../../../../../../lib/revocation-gates", () => ({
  claimRevocationGate: claimRevocationGateMock,
  releaseRevocationGateClaim: releaseRevocationGateClaimMock,
  revocationGateLockKey: vi.fn((target: {
    kind: string;
    organizationId: string;
    connectionId?: string;
    integrationId?: string;
  }) => `${target.kind}:${target.organizationId}:${
    target.connectionId ?? target.integrationId
  }`),
}));
vi.mock("../../../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { PUT } from "./route";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const connectionId = "22222222-2222-4222-8222-222222222222";
const integrationId = "33333333-3333-4333-8333-333333333333";
const claimId = "44444444-4444-4444-8444-444444444444";
const context = { params: Promise.resolve({ workspaceId, connectionId }) };
const claim = {
  kind: "connection" as const,
  organizationId: workspaceId,
  connectionId,
  claimId,
  claimedAt: new Date("2026-07-23T14:00:00.000Z"),
  pendingAt: new Date("2026-07-23T14:00:00.000Z"),
  firstPending: true,
  connectionRevision: 8,
};
const connection = {
  id: connectionId,
  organizationId: workspaceId,
  name: "Production",
  engine: "postgres",
  provider: "neon",
  driverId: null,
  host: "db.example.test",
  port: 5432,
  databaseName: "app",
  sslmode: "verify-full",
  readonlyDefault: true,
  allowWrites: true,
  credentialMode: "managed",
  providerIntegrationId: integrationId,
  providerResource: null,
  environment: "prod",
  schemaGroup: null,
  revision: 7,
  createdByUserId: "admin-user",
  createdAt: new Date("2026-07-20T00:00:00.000Z"),
  updatedAt: new Date("2026-07-20T00:00:00.000Z"),
  deletedAt: null,
  revocationPendingAt: null,
  revocationClaimedAt: null,
  revocationClaimId: null,
};

function mutationRequest(body: unknown) {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/connections/${connectionId}/managed-access`,
    {
      method: "PUT",
      headers: {
        "content-type": "application/json",
        origin: "https://app.example",
      },
      body: JSON.stringify(body),
    },
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  authorizeWorkspaceMock.mockResolvedValue({
    ok: true,
    session: { user: { id: "admin-user" } },
    role: "admin",
    accessMode: "manage",
  });
  connectionFindFirstMock.mockResolvedValue(connection);
  activeProviderIntegrationMock.mockResolvedValue({
    id: integrationId,
    organizationId: workspaceId,
    provider: "neon",
    encryptedCredential: "sealed",
    credentialExpiresAt: null,
  });
  validateManagedProviderResourceMock.mockResolvedValue(undefined);
  claimRevocationGateMock.mockResolvedValue(claim);
  releaseRevocationGateClaimMock.mockResolvedValue(true);
  revokeActiveLeasesMock.mockResolvedValue({ revoked: 1, deferred: 0 });
  batchMock.mockResolvedValue([
    {},
    [{ ...connection, credentialMode: "managed", revision: 8, updatedAt: new Date() }],
    {},
  ]);
});

describe("managed access revocation gate", () => {
  it("returns 409 without revoking when another mutation owns the connection gate", async () => {
    claimRevocationGateMock.mockResolvedValue(null);

    const response = await PUT(
      mutationRequest({ mode: "member_local" }),
      context,
    );

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: "Another connection access change is already in progress",
    });
    expect(revokeActiveLeasesMock).not.toHaveBeenCalled();
    expect(batchMock).not.toHaveBeenCalled();
  });

  it("releases the exact claim and returns 409 when revocation is deferred", async () => {
    revokeActiveLeasesMock.mockResolvedValue({ revoked: 0, deferred: 1 });

    const response = await PUT(
      mutationRequest({ mode: "member_local" }),
      context,
    );

    expect(response.status).toBe(409);
    expect(releaseRevocationGateClaimMock).toHaveBeenCalledOnce();
    expect(releaseRevocationGateClaimMock).toHaveBeenCalledWith(claim);
    expect(batchMock).not.toHaveBeenCalled();
  });

  it("guards the final update with the claim CAS and an active unblocked integration", async () => {
    const response = await PUT(
      mutationRequest({
        mode: "managed",
        integrationId,
        resource: {
          engine: "postgres",
          project: "project-id",
          branch: "branch-id",
          database: "app",
        },
      }),
      context,
    );

    expect(response.status).toBe(200);
    const condition = updateWhereMock.mock.calls.at(0)?.at(0) as SQL | undefined;
    expect(condition).toBeDefined();
    const compiled = new PgDialect().sqlToQuery(condition!);
    const normalized = compiled.sql.replace(/\s+/g, " ");

    expect(normalized).toContain(
      "\"workspace_connection\".\"revocation_claim_id\" = $",
    );
    expect(normalized).toContain(
      "\"workspace_provider_integration\".\"status\" = $",
    );
    expect(normalized).toContain(
      "\"workspace_provider_integration\".\"revoked_at\" is null",
    );
    expect(normalized).toContain(
      "\"workspace_provider_integration\".\"revocation_pending_at\" is null",
    );
    expect(compiled.params).toEqual(expect.arrayContaining([
      claimId,
      integrationId,
      "neon",
      "active",
    ]));
    expect(updateSetMock).toHaveBeenCalledWith(expect.objectContaining({
      revocationPendingAt: null,
      revocationClaimedAt: null,
      revocationClaimId: null,
    }));
  });
});
