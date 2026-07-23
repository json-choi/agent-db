import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  authorizeWorkspaceMock,
  batchMock,
  claimMock,
  connectionFindMock,
  releaseMock,
  revokeMock,
} = vi.hoisted(() => ({
  authorizeWorkspaceMock: vi.fn(),
  batchMock: vi.fn(),
  claimMock: vi.fn(),
  connectionFindMock: vi.fn(),
  releaseMock: vi.fn(),
  revokeMock: vi.fn(),
}));

vi.mock("server-only", () => ({}));
vi.mock("../../../../../../../lib/db", () => ({
  db: {
    batch: batchMock,
    query: { workspaceConnection: { findFirst: connectionFindMock } },
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
  releaseRevocationGateClaim: releaseMock,
}));
vi.mock("../../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { DELETE, PATCH } from "./route";

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

beforeEach(() => {
  vi.clearAllMocks();
  authorizeWorkspaceMock.mockResolvedValue({
    ok: true,
    role: "admin",
    accessMode: "manage",
    session: { user: { id: "admin-user" } },
  });
  connectionFindMock.mockResolvedValue({
    id: connectionId,
    engine: "postgres",
    provider: "generic",
    credentialMode: "member_local",
  });
  claimMock.mockResolvedValue(claim);
  releaseMock.mockResolvedValue(true);
  revokeMock.mockResolvedValue({ revoked: 0, deferred: 0 });
});

describe("connection authority mutation gate", () => {
  it("rejects a concurrent PATCH claimant before lease revocation", async () => {
    claimMock.mockResolvedValue(null);

    const response = await PATCH(request("PATCH"), context);

    expect(response.status).toBe(409);
    expect(revokeMock).not.toHaveBeenCalled();
    expect(batchMock).not.toHaveBeenCalled();
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
