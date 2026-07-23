import { beforeEach, describe, expect, it, vi } from "vitest";

const { getSessionMock, memberFindFirstMock } = vi.hoisted(() => ({
  getSessionMock: vi.fn(),
  memberFindFirstMock: vi.fn(),
}));

vi.mock("server-only", () => ({}));
vi.mock("./auth", () => ({
  auth: { api: { getSession: getSessionMock } },
}));
vi.mock("./db", () => ({
  db: {
    query: {
      member: { findFirst: memberFindFirstMock },
    },
  },
}));

import { authorizeWorkspace } from "./workspace-authorization";

const organizationId = "11111111-1111-4111-8111-111111111111";
const request = new Request("https://app.example/api/test");

beforeEach(() => {
  vi.clearAllMocks();
  getSessionMock.mockResolvedValue({ user: { id: "member-user" } });
  memberFindFirstMock.mockResolvedValue({
    id: "22222222-2222-4222-8222-222222222222",
    organizationId,
    userId: "member-user",
    role: "editor",
    revocationPendingAt: null,
  });
});

describe("workspace authorization revocation gate", () => {
  it("allows an active member with the requested capability", async () => {
    await expect(authorizeWorkspace(request, organizationId, "write"))
      .resolves.toMatchObject({
        ok: true,
        role: "editor",
        accessMode: "write",
      });
  });

  it("fails closed while a member authority change is pending", async () => {
    memberFindFirstMock.mockResolvedValue({
      id: "22222222-2222-4222-8222-222222222222",
      organizationId,
      userId: "member-user",
      role: "editor",
      revocationPendingAt: new Date(),
    });

    await expect(authorizeWorkspace(request, organizationId, "read"))
      .resolves.toEqual({
        ok: false,
        status: 403,
        error: "Workspace access denied",
      });
  });
});
