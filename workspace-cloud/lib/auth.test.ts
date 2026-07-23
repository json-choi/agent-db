import { describe, expect, it, vi } from "vitest";

vi.mock("server-only", () => ({}));
vi.mock("./db", () => ({ db: {} }));
vi.mock("./env", () => ({
  env: {
    appOrigin: () => "https://app.example",
    authSecret: () => "test-secret-that-is-at-least-thirty-two-characters",
    googleClientId: () => "000000000000-test.apps.googleusercontent.com",
    googleClientSecret: () => "test-google-secret",
  },
}));
vi.mock("./invitation-email", () => ({
  sendWorkspaceInvitation: vi.fn(),
}));

import { auth } from "./auth";

describe("Better Auth membership mutation boundary", () => {
  it.each([
    "/organization/update-member-role",
    "/organization/remove-member",
    "/organization/leave",
  ])("blocks the public HTTP endpoint %s", async (path) => {
    const response = await auth.handler(new Request(
      `https://app.example/api/auth${path}`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      },
    ));

    expect(response.status).toBe(404);
  });

  it("keeps trusted server-side member mutation methods available", () => {
    expect(auth.api.updateMemberRole).toBeTypeOf("function");
    expect(auth.api.removeMember).toBeTypeOf("function");
  });
});
