// PlanetScale OAuth callback tests verify that reconnecting an active integration
// drains leases before atomically rotating credentials and invalidating connections.
import type { SQL } from "drizzle-orm";
import { PgDialect } from "drizzle-orm/pg-core";
import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  authorizeWorkspaceMock,
  batchMock,
  claimMock,
  deleteReturningMock,
  exchangeCodeMock,
  executeMock,
  findIntegrationMock,
  getSessionMock,
  insertMock,
  inspectTokenMock,
  releaseMock,
  revokeAuthorizationMock,
  revokeLeasesMock,
  revokePlanetScaleMock,
  sealCredentialMock,
  updateMock,
  updateRecords,
} = vi.hoisted(() => ({
  authorizeWorkspaceMock: vi.fn(),
  batchMock: vi.fn(),
  claimMock: vi.fn(),
  deleteReturningMock: vi.fn(),
  exchangeCodeMock: vi.fn(),
  executeMock: vi.fn((statement: unknown): unknown => ({ statement })),
  findIntegrationMock: vi.fn(),
  getSessionMock: vi.fn(),
  insertMock: vi.fn(),
  inspectTokenMock: vi.fn(),
  releaseMock: vi.fn(),
  revokeAuthorizationMock: vi.fn(),
  revokeLeasesMock: vi.fn(),
  revokePlanetScaleMock: vi.fn(),
  sealCredentialMock: vi.fn(),
  updateMock: vi.fn(),
  updateRecords: [] as Array<{ values: Record<string, unknown>; where: SQL }>,
}));

function deleteBuilder() {
  const builder = {
    where: vi.fn(),
    returning: vi.fn(),
  };
  builder.where.mockReturnValue(builder);
  builder.returning.mockImplementation(() => deleteReturningMock());
  return builder;
}

function updateBuilder(token: unknown) {
  return {
    set: vi.fn((values: Record<string, unknown>) => ({
      where: vi.fn((where: SQL) => ({
        returning: vi.fn(() => {
          updateRecords.push({ values, where });
          return token;
        }),
      })),
    })),
  };
}

function insertBuilder(token: unknown) {
  return {
    values: vi.fn(() => token),
  };
}

vi.mock("server-only", () => ({}));
vi.mock("../../../../../../lib/auth", () => ({
  auth: { api: { getSession: getSessionMock } },
}));
vi.mock("../../../../../../lib/db", () => ({
  db: {
    batch: batchMock,
    delete: vi.fn(() => deleteBuilder()),
    execute: executeMock,
    insert: insertMock,
    query: {
      workspaceProviderIntegration: {
        findFirst: findIntegrationMock,
      },
    },
    update: updateMock,
  },
}));
vi.mock("../../../../../../lib/env", () => ({
  env: { appOrigin: () => "https://app.example" },
}));
vi.mock("../../../../../../lib/providers/planetscale", () => ({
  exchangePlanetScaleCode: exchangeCodeMock,
  inspectPlanetScaleToken: inspectTokenMock,
  revokePlanetScaleAuthorization: revokePlanetScaleMock,
}));
vi.mock("../../../../../../lib/provider-integrations", () => ({
  revokeActiveLeases: revokeLeasesMock,
  revokeProviderAuthorization: revokeAuthorizationMock,
}));
vi.mock("../../../../../../lib/revocation-gates", () => ({
  claimRevocationGate: claimMock,
  releaseRevocationGateClaim: releaseMock,
}));
vi.mock("../../../../../../lib/secret-envelope", () => ({
  sealProviderCredential: sealCredentialMock,
}));
vi.mock("../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { GET } from "./route";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const integrationId = "22222222-2222-4222-8222-222222222222";
const claim = {
  kind: "integration",
  organizationId: workspaceId,
  integrationId,
  claimId: "33333333-3333-4333-8333-333333333333",
  claimedAt: new Date("2026-07-23T00:00:00Z"),
  pendingAt: new Date("2026-07-23T00:00:00Z"),
  firstPending: true,
};
const existing = {
  id: integrationId,
  organizationId: workspaceId,
  provider: "planetScale",
  encryptedCredential: "sealed-old",
  credentialExpiresAt: new Date("2026-07-24T00:00:00Z"),
  status: "active",
  revokedAt: null,
  revocationPendingAt: null,
  updatedAt: new Date("2026-07-23T00:00:00Z"),
};
const managedScope = [
  "read_organizations",
  "read_databases",
  "read_branches",
  "manage_passwords",
  "manage_production_branch_passwords",
].join(" ");
const token = {
  accessToken: "new-access-token",
  refreshToken: "new-refresh-token",
  expiresAt: "2026-07-24T01:00:00.000Z",
  scope: managedScope,
};

function request() {
  const url = new URL(
    "/api/v1/providers/planet-scale/callback",
    "https://app.example",
  );
  url.searchParams.set("state", "s".repeat(43));
  url.searchParams.set("code", "valid-code");
  return new Request(url);
}

function redirectStatus(response: Response) {
  return new URL(response.headers.get("location") ?? "https://invalid").searchParams
    .get("status");
}

function renderedSql(statement: SQL) {
  return new PgDialect().sqlToQuery(statement).sql.replace(/\s+/g, " ");
}

beforeEach(() => {
  vi.clearAllMocks();
  updateRecords.splice(0);
  getSessionMock.mockResolvedValue({ user: { id: "admin-user" } });
  deleteReturningMock.mockResolvedValue([{ organizationId: workspaceId }]);
  authorizeWorkspaceMock.mockResolvedValue({
    ok: true,
    session: { user: { id: "admin-user" } },
  });
  exchangeCodeMock.mockResolvedValue(token);
  inspectTokenMock.mockResolvedValue({
    subject: "org-account-subject",
    scope: managedScope,
    expiresAt: token.expiresAt,
  });
  findIntegrationMock.mockResolvedValue(existing);
  sealCredentialMock.mockReturnValue("sealed-new");
  claimMock.mockResolvedValue(claim);
  releaseMock.mockResolvedValue(true);
  revokeLeasesMock.mockResolvedValue({ revoked: 2, deferred: 0 });
  revokeAuthorizationMock.mockResolvedValue(undefined);
  revokePlanetScaleMock.mockResolvedValue(undefined);
  executeMock.mockReset().mockImplementation(
    (statement: unknown) => ({ statement }),
  );
  updateMock.mockReset()
    .mockImplementationOnce(() => updateBuilder({ kind: "updated" }) as never)
    .mockImplementationOnce(() => updateBuilder({ kind: "cleared" }) as never);
  insertMock.mockReset()
    .mockImplementation(() => insertBuilder({ kind: "inserted" }) as never);
  batchMock.mockResolvedValue([
    [{ id: integrationId }],
    { rows: [] },
    { rows: [] },
    [{ id: integrationId }],
  ]);
});

describe("PlanetScale OAuth reconnect gate", () => {
  it("drains leases before an atomic credential rotation and revision bump", async () => {
    const response = await GET(request());

    expect(response.status).toBe(302);
    expect(redirectStatus(response)).toBe("connected");
    expect(claimMock).toHaveBeenCalledWith({
      kind: "integration",
      organizationId: workspaceId,
      integrationId,
    });
    expect(revokeLeasesMock).toHaveBeenCalledWith({
      organizationId: workspaceId,
      integrationId,
    });
    expect(claimMock.mock.invocationCallOrder[0]).toBeLessThan(
      revokeLeasesMock.mock.invocationCallOrder[0],
    );
    expect(revokeLeasesMock.mock.invocationCallOrder[0]).toBeLessThan(
      batchMock.mock.invocationCallOrder[0],
    );

    const statements = executeMock.mock.calls.map(([statement]) => (
      renderedSql(statement as SQL)
    ));
    expect(statements.some((query) => (
      query.includes("UPDATE \"workspace_control\".\"workspace_connection\"")
      && query.includes("\"revision\" = connection.\"revision\" + 1")
      && query.includes("\"provider_integration_id\" = integration.\"id\"")
      && query.includes("connection.\"deleted_at\" IS NULL")
      && query.includes("integration.\"revocation_claim_id\" =")
    ))).toBe(true);
    expect(statements.some((query) => (
      query.includes("INSERT INTO \"workspace_control\".\"workspace_audit_event\"")
      && query.includes("integration.\"revocation_claim_id\" =")
    ))).toBe(true);

    expect(updateRecords).toHaveLength(2);
    const updateGuards = updateRecords.map(({ where }) => renderedSql(where));
    expect(updateGuards[0]).toContain("\"revocation_claim_id\" =");
    expect(updateGuards[0]).toContain("\"status\" =");
    expect(updateGuards[0]).toContain("\"revoked_at\" is null");
    expect(updateRecords[0]?.values).not.toHaveProperty("revocationClaimId");
    expect(updateGuards[1]).toContain("\"revocation_claim_id\" =");
    expect(updateGuards[1]).toContain("\"updated_at\" =");
    expect(updateRecords[1]?.values).toMatchObject({
      revocationPendingAt: null,
      revocationClaimedAt: null,
      revocationClaimId: null,
    });
    expect(batchMock.mock.calls[0]?.[0]).toHaveLength(4);
    expect(releaseMock).not.toHaveBeenCalled();
    expect(revokeAuthorizationMock).toHaveBeenCalledWith(existing);
    expect(batchMock.mock.invocationCallOrder[0]).toBeLessThan(
      revokeAuthorizationMock.mock.invocationCallOrder[0],
    );
    expect(revokePlanetScaleMock).not.toHaveBeenCalled();
    expect(deleteReturningMock.mock.invocationCallOrder[0]).toBeLessThan(
      exchangeCodeMock.mock.invocationCallOrder[0],
    );
  });

  it("keeps the old credential when a lease cleanup is deferred", async () => {
    revokeLeasesMock.mockResolvedValue({ revoked: 1, deferred: 1 });

    const response = await GET(request());

    expect(redirectStatus(response)).toBe("failed");
    expect(releaseMock).toHaveBeenCalledTimes(1);
    expect(releaseMock).toHaveBeenCalledWith(claim);
    expect(batchMock).not.toHaveBeenCalled();
    expect(revokeAuthorizationMock).not.toHaveBeenCalled();
    expect(revokePlanetScaleMock).toHaveBeenCalledWith(token.refreshToken);
  });

  it("fails closed when another revocation claimant owns the integration", async () => {
    claimMock.mockResolvedValue(null);

    const response = await GET(request());

    expect(redirectStatus(response)).toBe("failed");
    expect(revokeLeasesMock).not.toHaveBeenCalled();
    expect(batchMock).not.toHaveBeenCalled();
    expect(releaseMock).not.toHaveBeenCalled();
    expect(revokeAuthorizationMock).not.toHaveBeenCalled();
    expect(revokePlanetScaleMock).toHaveBeenCalledWith(token.refreshToken);
  });

  it("releases its exact claim and revokes the new token on a stale batch", async () => {
    batchMock.mockResolvedValue([
      [],
      { rows: [] },
      { rows: [] },
      [],
    ]);

    const response = await GET(request());

    expect(redirectStatus(response)).toBe("failed");
    expect(releaseMock).toHaveBeenCalledTimes(1);
    expect(releaseMock).toHaveBeenCalledWith(claim);
    expect(revokeAuthorizationMock).not.toHaveBeenCalled();
    expect(revokePlanetScaleMock).toHaveBeenCalledWith(token.refreshToken);
  });

  it("reactivates an inactive integration only through its exact CAS snapshot", async () => {
    const revokedAt = new Date("2026-07-22T23:00:00Z");
    findIntegrationMock.mockResolvedValue({
      ...existing,
      status: "revoked",
      revokedAt,
    });

    const response = await GET(request());

    expect(redirectStatus(response)).toBe("connected");
    expect(claimMock).not.toHaveBeenCalled();
    expect(revokeLeasesMock).not.toHaveBeenCalled();
    expect(batchMock.mock.calls[0]?.[0]).toHaveLength(3);
    expect(updateRecords).toHaveLength(1);
    const updateGuard = renderedSql(updateRecords[0]!.where);
    expect(updateGuard).toContain("\"revocation_pending_at\" is null");
    expect(updateGuard).toContain("\"revocation_claim_id\" is null");
    expect(updateGuard).toContain("\"status\" =");
    expect(updateGuard).toContain("\"updated_at\" =");
    expect(updateGuard).toContain("\"revoked_at\" =");
    const statements = executeMock.mock.calls.map(([statement]) => (
      renderedSql(statement as SQL)
    ));
    expect(statements.some((query) => (
      query.includes("UPDATE \"workspace_control\".\"workspace_connection\"")
      && query.includes("\"revision\" = connection.\"revision\" + 1")
      && query.includes("integration.\"revocation_pending_at\" IS NULL")
      && query.includes("integration.\"revocation_claim_id\" IS NULL")
    ))).toBe(true);
    expect(releaseMock).not.toHaveBeenCalled();
    expect(revokeAuthorizationMock).toHaveBeenCalled();
    expect(revokePlanetScaleMock).not.toHaveBeenCalled();
  });
});
