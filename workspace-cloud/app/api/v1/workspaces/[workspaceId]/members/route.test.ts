import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  authorizeWorkspaceMock,
  claimRevocationGateMock,
  executeMock,
  memberFindFirstMock,
  releaseRevocationGateClaimMock,
  renewRevocationGateClaimMock,
  revokeActiveLeasesMock,
} = vi.hoisted(() => ({
  authorizeWorkspaceMock: vi.fn(),
  claimRevocationGateMock: vi.fn(),
  executeMock: vi.fn(),
  memberFindFirstMock: vi.fn(),
  releaseRevocationGateClaimMock: vi.fn(),
  renewRevocationGateClaimMock: vi.fn(),
  revokeActiveLeasesMock: vi.fn(),
}));

vi.mock("../../../../../../lib/auth", () => ({
  auth: {
    api: {
      cancelInvitation: vi.fn(),
      createInvitation: vi.fn(),
    },
  },
}));
vi.mock("../../../../../../lib/db", () => ({
  db: {
    execute: executeMock,
    insert: vi.fn(),
    query: {
      invitation: { findFirst: vi.fn() },
      member: { findFirst: memberFindFirstMock },
    },
    select: vi.fn(),
  },
}));
vi.mock("../../../../../../lib/env", () => ({
  env: { appOrigin: () => "https://app.example" },
}));
vi.mock("../../../../../../lib/provider-integrations", () => ({
  revokeActiveLeases: revokeActiveLeasesMock,
}));
vi.mock("../../../../../../lib/revocation-gates", () => ({
  claimRevocationGate: claimRevocationGateMock,
  releaseRevocationGateClaim: releaseRevocationGateClaimMock,
  renewRevocationGateClaim: renewRevocationGateClaimMock,
}));
vi.mock("../../../../../../lib/workspace-authorization", () => ({
  authorizeWorkspace: authorizeWorkspaceMock,
}));

import { DELETE, PATCH } from "./route";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const memberId = "22222222-2222-4222-8222-222222222222";
const initialClaimId = "33333333-3333-4333-8333-333333333333";
const renewedClaimId = "44444444-4444-4444-8444-444444444444";
const claimedAt = new Date("2026-07-23T12:00:00.000Z");
const context = { params: Promise.resolve({ workspaceId }) };
const initialClaim = {
  kind: "member" as const,
  organizationId: workspaceId,
  memberId,
  userId: "target-user",
  claimId: initialClaimId,
  claimedAt,
  pendingAt: claimedAt,
  firstPending: true,
};
const renewedClaim = {
  ...initialClaim,
  claimId: renewedClaimId,
  claimedAt: new Date("2026-07-23T12:00:01.000Z"),
};

function mutationRequest(method: "PATCH" | "DELETE", body: unknown) {
  return new Request(
    `https://app.example/api/v1/workspaces/${workspaceId}/members`,
    {
      method,
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
  });
  memberFindFirstMock.mockResolvedValue({
    id: memberId,
    organizationId: workspaceId,
    userId: "target-user",
    role: "editor",
  });
  claimRevocationGateMock.mockResolvedValue(initialClaim);
  renewRevocationGateClaimMock.mockResolvedValue(renewedClaim);
  releaseRevocationGateClaimMock.mockResolvedValue(true);
  revokeActiveLeasesMock.mockResolvedValue({ revoked: 1, deferred: 0 });
  executeMock.mockResolvedValue({ rows: [] });
});

describe("workspace member lease revocation gate", () => {
  it("keeps a pending role-change gate while managed lease revocation is deferred", async () => {
    revokeActiveLeasesMock.mockResolvedValue({ revoked: 1, deferred: 1 });

    const response = await PATCH(
      mutationRequest("PATCH", { memberId, role: "viewer" }),
      context,
    );

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: "Active database access could not be revoked; retry after its lease expires",
    });
    expect(claimRevocationGateMock).toHaveBeenCalledWith({
      kind: "member",
      organizationId: workspaceId,
      memberId,
      userId: "target-user",
    });
    expect(revokeActiveLeasesMock).toHaveBeenCalledWith({
      organizationId: workspaceId,
      userId: "target-user",
    });
    expect(releaseRevocationGateClaimMock).toHaveBeenCalledWith(initialClaim);
    expect(renewRevocationGateClaimMock).not.toHaveBeenCalled();
    expect(executeMock).not.toHaveBeenCalled();
  });

  it("keeps a pending removal gate while managed lease revocation is deferred", async () => {
    revokeActiveLeasesMock.mockResolvedValue({ revoked: 0, deferred: 1 });

    const response = await DELETE(
      mutationRequest("DELETE", { memberId }),
      context,
    );

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: "Active database access could not be revoked; retry after its lease expires",
    });
    expect(releaseRevocationGateClaimMock).toHaveBeenCalledWith(initialClaim);
    expect(renewRevocationGateClaimMock).not.toHaveBeenCalled();
    expect(executeMock).not.toHaveBeenCalled();
  });

  it("renews UUID ownership before atomically updating the role and audit", async () => {
    executeMock.mockResolvedValue({
      rows: [{
        id: memberId,
        organizationId: workspaceId,
        userId: "target-user",
        role: "viewer",
        createdAt: "2026-07-23T00:00:00.000Z",
      }],
    });

    const response = await PATCH(
      mutationRequest("PATCH", { memberId, role: "viewer" }),
      context,
    );

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      member: {
        id: memberId,
        organizationId: workspaceId,
        userId: "target-user",
        role: "viewer",
      },
    });
    expect(renewRevocationGateClaimMock).toHaveBeenCalledWith(initialClaim);
    expect(executeMock).toHaveBeenCalledOnce();
    expect(releaseRevocationGateClaimMock).not.toHaveBeenCalled();
    expect(claimRevocationGateMock.mock.invocationCallOrder[0])
      .toBeLessThan(revokeActiveLeasesMock.mock.invocationCallOrder[0]);
    expect(revokeActiveLeasesMock.mock.invocationCallOrder[0])
      .toBeLessThan(renewRevocationGateClaimMock.mock.invocationCallOrder[0]);
    expect(renewRevocationGateClaimMock.mock.invocationCallOrder[0])
      .toBeLessThan(executeMock.mock.invocationCallOrder[0]);
  });

  it("deletes a member only through a renewed UUID-owned SQL mutation", async () => {
    executeMock.mockResolvedValue({ rows: [{ id: memberId }] });

    const response = await DELETE(
      mutationRequest("DELETE", { memberId }),
      context,
    );

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({ status: true });
    expect(renewRevocationGateClaimMock).toHaveBeenCalledWith(initialClaim);
    expect(executeMock).toHaveBeenCalledOnce();
    expect(releaseRevocationGateClaimMock).not.toHaveBeenCalled();
  });

  it("fails before revocation when another UUID claim owns the member gate", async () => {
    claimRevocationGateMock.mockResolvedValue(null);

    const response = await PATCH(
      mutationRequest("PATCH", { memberId, role: "viewer" }),
      context,
    );

    expect(response.status).toBe(409);
    expect(revokeActiveLeasesMock).not.toHaveBeenCalled();
    expect(renewRevocationGateClaimMock).not.toHaveBeenCalled();
    expect(executeMock).not.toHaveBeenCalled();
  });

  it("does not report success when the renewed UUID loses its final SQL CAS", async () => {
    const response = await PATCH(
      mutationRequest("PATCH", { memberId, role: "viewer" }),
      context,
    );

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: "Member access changed concurrently. Retry the update.",
    });
    expect(releaseRevocationGateClaimMock).toHaveBeenCalledWith(renewedClaim);
  });
});
