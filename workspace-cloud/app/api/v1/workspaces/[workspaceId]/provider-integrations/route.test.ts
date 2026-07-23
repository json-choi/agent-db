// GCP provider setup tests cover global claim conflicts and the atomic
// reconnect paths without ever persisting or logging service-account emails.
import { PgDialect } from "drizzle-orm/pg-core";
import type { SQL } from "drizzle-orm";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  gcpCloudSqlIntegrationIdentity,
  parseGcpCloudSqlCredential,
} from "../../../../../../lib/providers/gcp-cloud-sql-core";

const {
  authorizeWorkspaceMock,
  batchMock,
  claimMock,
  executeMock,
  findIntegrationMock,
  insertMock,
  releaseMock,
  revokeLeasesMock,
  selectResults,
  updateMock,
  validateGcpMock,
} = vi.hoisted(() => ({
  authorizeWorkspaceMock: vi.fn(),
  batchMock: vi.fn(),
  claimMock: vi.fn(),
  executeMock: vi.fn((statement: unknown): unknown => ({ statement })),
  findIntegrationMock: vi.fn(),
  insertMock: vi.fn(),
  releaseMock: vi.fn(),
  revokeLeasesMock: vi.fn(),
  selectResults: [] as unknown[][],
  updateMock: vi.fn(),
  validateGcpMock: vi.fn(),
}));

function selectBuilder() {
  const builder = {
    from: vi.fn(),
    innerJoin: vi.fn(),
    where: vi.fn(),
  };
  builder.from.mockReturnValue(builder);
  builder.innerJoin.mockReturnValue(builder);
  builder.where.mockImplementation(async () => selectResults.shift() ?? []);
  return builder;
}

function updateBuilder(token: unknown) {
  return {
    set: vi.fn(() => ({
      where: vi.fn(() => ({
        returning: vi.fn(() => token),
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
vi.mock("../../../../../../lib/db", () => ({
  db: {
    select: vi.fn(() => selectBuilder()),
    query: {
      workspaceProviderIntegration: {
        findFirst: findIntegrationMock,
      },
    },
    batch: batchMock,
    execute: executeMock,
    update: updateMock,
    insert: insertMock,
  },
}));
vi.mock("../../../../../../lib/env", () => ({
  env: { appOrigin: () => "https://app.example" },
}));
vi.mock("../../../../../../lib/provider-integrations", () => ({
  parseManagedProviderResource: vi.fn(),
  revokeActiveLeases: revokeLeasesMock,
}));
vi.mock("../../../../../../lib/revocation-gates", () => ({
  claimRevocationGate: claimMock,
  releaseRevocationGateClaim: releaseMock,
}));
vi.mock("../../../../../../lib/providers/planetscale", () => ({
  isPlanetScaleConfigured: () => false,
  planetScaleAuthorizationUrl: vi.fn(),
  PlanetScaleRequestError: class PlanetScaleRequestError extends Error {},
}));
vi.mock("../../../../../../lib/providers/neon", () => ({
  inspectNeonCredential: vi.fn(),
}));
vi.mock("../../../../../../lib/providers/gcp-cloud-sql", () => ({
  validateGcpCloudSqlCredential: validateGcpMock,
  vercelOidcToken: () => "oidc-token",
}));
vi.mock("../../../../../../lib/secret-envelope", () => ({
  openProviderCredential: vi.fn(),
  sealProviderCredential: () => "sealed",
}));
vi.mock("../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { POST } from "./route";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const integrationId = "22222222-2222-4222-8222-222222222222";
const context = { params: Promise.resolve({ workspaceId }) };
const configuration = {
  projectId: "sample-project-123",
  projectNumber: "123456789012",
  workloadIdentityPoolId: "vercel-prod",
  workloadIdentityProviderId: "dopedb-app",
  instanceId: "prod-db",
  readServiceAccountEmail:
    "dopedb-read@sample-project-123.iam.gserviceaccount.com",
  writeServiceAccountEmail:
    "dopedb-write@sample-project-123.iam.gserviceaccount.com",
  dedicatedServiceAccountsConfirmed: true,
  instanceScopedIamConfirmed: true,
};
const identity = gcpCloudSqlIntegrationIdentity(
  parseGcpCloudSqlCredential(configuration),
);

function request() {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/provider-integrations`,
    {
      method: "POST",
      headers: {
        origin: "https://app.example",
        "content-type": "application/json",
      },
      body: JSON.stringify({ provider: "gcpCloudSql", configuration }),
    },
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  selectResults.splice(0);
  executeMock.mockReset().mockImplementation(
    (statement: unknown) => ({ statement }),
  );
  authorizeWorkspaceMock.mockResolvedValue({
    ok: true,
    session: { user: { id: "admin-user" } },
  });
  validateGcpMock.mockResolvedValue(undefined);
  revokeLeasesMock.mockResolvedValue({ revoked: 0, deferred: 0 });
  releaseMock.mockResolvedValue(true);
  findIntegrationMock.mockResolvedValue(undefined);
  updateMock.mockReset()
    .mockImplementationOnce(() => updateBuilder({ kind: "prepared" }) as never)
    .mockImplementationOnce(() => updateBuilder({ kind: "cleared" }) as never);
  insertMock.mockReset()
    .mockImplementationOnce(() => insertBuilder({ kind: "integration" }) as never)
    .mockImplementationOnce(() => insertBuilder({ kind: "claims" }) as never)
    .mockImplementationOnce(() => insertBuilder({ kind: "audit" }) as never);
});

describe("GCP provider principal claims", () => {
  it("rejects a service account already claimed by another workspace", async () => {
    selectResults.push([{
      principalFingerprint: identity.readPrincipal,
      targetFingerprint: identity.instance,
      integrationId,
      organizationId: "99999999-9999-4999-8999-999999999999",
      provider: "gcpCloudSql",
      status: "active",
      revokedAt: null,
      revocationPendingAt: null,
      updatedAt: new Date(),
    }]);

    const response = await POST(request(), context);

    expect(response.status).toBe(409);
    expect(batchMock).not.toHaveBeenCalled();
    expect(findIntegrationMock).not.toHaveBeenCalled();
  });

  it("maps an atomic principal or target collision to a retry-safe conflict", async () => {
    selectResults.push([], []);
    batchMock.mockRejectedValue(Object.assign(new Error("duplicate"), {
      code: "23505",
      constraint: "provider_principal_claim_org_target_idx",
    }));

    const response = await POST(request(), context);

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toMatchObject({
      error: expect.stringMatching(/service accounts or target/),
    });
  });

  it("replaces claims inside the active-integration CAS batch", async () => {
    const updatedAt = new Date("2026-07-23T00:00:00Z");
    const existing = {
      id: integrationId,
      status: "active",
      revokedAt: null,
      revocationPendingAt: null,
      updatedAt,
    };
    selectResults.push(
      [{
        principalFingerprint: identity.readPrincipal,
        targetFingerprint: identity.instance,
        integrationId,
        organizationId: workspaceId,
        provider: "gcpCloudSql",
        ...existing,
      }],
      [existing],
    );
    const claim = {
      kind: "integration",
      organizationId: workspaceId,
      integrationId,
      claimId: "33333333-3333-4333-8333-333333333333",
      claimedAt: new Date(),
      pendingAt: new Date(),
      firstPending: true,
    };
    claimMock.mockResolvedValue(claim);
    batchMock.mockResolvedValue([
      [{ id: integrationId }],
      { rows: [] },
      { rows: [] },
      { rows: [] },
      [{ id: integrationId }],
    ]);

    const response = await POST(request(), context);

    expect(response.status).toBe(200);
    const statements = executeMock.mock.calls.map(([statement]) => (
      new PgDialect().sqlToQuery(statement as SQL).sql.replace(/\s+/g, " ")
    ));
    expect(statements.some((query) => (
      query.includes("DELETE FROM \"workspace_control\"."
        + "\"workspace_provider_principal_claim\"")
      && query.includes("\"revocation_claim_id\" =")
    ))).toBe(true);
    expect(statements.some((query) => (
      query.includes("INSERT INTO \"workspace_control\"."
        + "\"workspace_provider_principal_claim\"")
      && query.includes("VALUES")
    ))).toBe(true);
    expect(batchMock).toHaveBeenCalledTimes(1);
    expect(releaseMock).not.toHaveBeenCalled();
  });

  it("reactivates an exact revoked row with one CAS CTE", async () => {
    selectResults.push([], []);
    findIntegrationMock.mockResolvedValue({
      id: integrationId,
      status: "revoked",
      revokedAt: new Date("2026-07-22T23:00:00Z"),
      revocationPendingAt: null,
      updatedAt: new Date("2026-07-22T23:00:00Z"),
    });
    executeMock.mockResolvedValue({ rows: [{ id: integrationId }] });

    const response = await POST(request(), context);

    expect(response.status).toBe(200);
    const statement = executeMock.mock.calls[0]?.[0] as SQL;
    const query = new PgDialect().sqlToQuery(statement).sql.replace(/\s+/g, " ");
    expect(query).toContain("WITH updated_integration AS");
    expect(query).toContain("inserted_claims AS");
    expect(query).toContain("audit_event AS");
    expect(query).toContain("\"status\" = $");
    expect(query).toContain("\"updated_at\" = $");
    expect(query).not.toContain("@sample-project-123");
    expect(batchMock).not.toHaveBeenCalled();
  });
});
