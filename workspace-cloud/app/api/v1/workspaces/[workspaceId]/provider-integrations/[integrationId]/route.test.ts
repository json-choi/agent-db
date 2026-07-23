import { PgDialect } from "drizzle-orm/pg-core";
import type { SQL } from "drizzle-orm";
import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  authorizeWorkspaceMock,
  claimMock,
  executeMock,
  integrationForRevocationMock,
  releaseMock,
  revokeAuthorizationMock,
  revokeLeasesMock,
} = vi.hoisted(() => ({
  authorizeWorkspaceMock: vi.fn(),
  claimMock: vi.fn(),
  executeMock: vi.fn(),
  integrationForRevocationMock: vi.fn(),
  releaseMock: vi.fn(),
  revokeAuthorizationMock: vi.fn(),
  revokeLeasesMock: vi.fn(),
}));

vi.mock("server-only", () => ({}));
vi.mock("../../../../../../../lib/db", () => ({
  db: { execute: executeMock },
}));
vi.mock("../../../../../../../lib/env", () => ({
  env: { appOrigin: () => "https://app.example" },
}));
vi.mock("../../../../../../../lib/provider-integrations", () => ({
  providerIntegrationForRevocation: integrationForRevocationMock,
  revokeActiveLeases: revokeLeasesMock,
  revokeProviderAuthorization: revokeAuthorizationMock,
}));
vi.mock("../../../../../../../lib/revocation-gates", () => ({
  claimRevocationGate: claimMock,
  releaseRevocationGateClaim: releaseMock,
}));
vi.mock("../../../../../../../lib/secret-envelope", () => ({
  sealProviderCredential: () => "scrubbed",
}));
vi.mock("../../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { DELETE } from "./route";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const integrationId = "22222222-2222-4222-8222-222222222222";
const context = { params: Promise.resolve({ workspaceId, integrationId }) };
const claim = {
  kind: "integration",
  organizationId: workspaceId,
  integrationId,
  claimId: "33333333-3333-4333-8333-333333333333",
  claimedAt: new Date("2026-07-23T00:00:00Z"),
  pendingAt: new Date("2026-07-23T00:00:00Z"),
  firstPending: true,
};
const integration = {
  id: integrationId,
  organizationId: workspaceId,
  provider: "neon",
  encryptedCredential: "sealed",
  credentialExpiresAt: null,
};

function request() {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}`
      + `/provider-integrations/${integrationId}`,
    { method: "DELETE", headers: { origin: "https://app.example" } },
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  authorizeWorkspaceMock.mockResolvedValue({
    ok: true,
    session: { user: { id: "admin-user" } },
  });
  claimMock.mockResolvedValue(claim);
  integrationForRevocationMock.mockResolvedValue(integration);
  releaseMock.mockResolvedValue(true);
  revokeLeasesMock.mockResolvedValue({ revoked: 2, deferred: 0 });
  revokeAuthorizationMock.mockResolvedValue(undefined);
  executeMock.mockResolvedValue({ rows: [{ id: integrationId }] });
});

describe("provider disconnect authority gate", () => {
  it("rejects a concurrent claimant before reading provider credentials", async () => {
    claimMock.mockResolvedValue(null);

    const response = await DELETE(request(), context);

    expect(response.status).toBe(409);
    expect(revokeLeasesMock).not.toHaveBeenCalled();
    expect(revokeAuthorizationMock).not.toHaveBeenCalled();
  });

  it("keeps the integration pending when a live lease cannot be revoked", async () => {
    revokeLeasesMock.mockResolvedValue({ revoked: 1, deferred: 1 });

    const response = await DELETE(request(), context);

    expect(response.status).toBe(409);
    expect(releaseMock).toHaveBeenCalledWith(claim);
    expect(revokeAuthorizationMock).not.toHaveBeenCalled();
    expect(executeMock).not.toHaveBeenCalled();
  });

  it("atomically CASes integration revocation, detach, and audit", async () => {
    const response = await DELETE(request(), context);

    expect(response.status).toBe(204);
    const statement = executeMock.mock.calls[0]?.[0] as SQL;
    const query = new PgDialect().sqlToQuery(statement).sql.replace(/\s+/g, " ");
    expect(query).toContain("WITH revoked_integration AS");
    expect(query).toContain("\"revocation_claim_id\" = $");
    expect(query).toContain("detached_connections AS");
    expect(query).toContain("deleted_principal_claims AS");
    expect(query).toContain(
      "DELETE FROM \"workspace_control\".\"workspace_provider_principal_claim\"",
    );
    expect(query).toContain("audit_event AS");
    expect(releaseMock).not.toHaveBeenCalled();
  });
});
