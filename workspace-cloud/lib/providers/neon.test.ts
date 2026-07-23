// Contract tests for Neon API-key identity resolution. Project-scoped keys cannot
// call account endpoints, so the adapter must use the supported organization path.
import { afterEach, describe, expect, it, vi } from "vitest";
import { neonIntegrationIdentity } from "./neon-core";
import { inspectNeonCredential } from "./neon";

vi.mock("server-only", () => ({}));

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("Neon API identity", () => {
  it("supports a project-scoped key without the removed /auth endpoint", async () => {
    const paths: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request) => {
      const url = new URL(String(input));
      paths.push(url.pathname);
      if (url.pathname === "/api/v2/projects") {
        return Response.json({
          projects: [{ id: "quiet-field-123", name: "Production" }],
        });
      }
      if (url.pathname === "/api/v2/users/me") {
        return Response.json({ error: "project scoped" }, { status: 403 });
      }
      if (url.pathname === "/api/v2/users/me/organizations") {
        return Response.json({
          organizations: [{ id: "org-safe-123", name: "Safe" }],
        });
      }
      return Response.json({ error: "unexpected" }, { status: 404 });
    }));

    const info = await inspectNeonCredential({
      apiKey: "napi_".padEnd(64, "a"),
      organizationId: null,
    });
    expect(info.externalAccountId).toBe(
      neonIntegrationIdentity(
        { kind: "organization", id: "org-safe-123" },
        ["quiet-field-123"],
      ).externalAccountId,
    );
    expect(paths).toContain("/api/v2/users/me");
    expect(paths).toContain("/api/v2/users/me/organizations");
    expect(paths).not.toContain("/api/v2/auth");
  });

  it("maps a revoked Neon key away from workspace-session 401", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => (
      Response.json({ error: "revoked" }, { status: 401 })
    )));
    await expect(inspectNeonCredential({
      apiKey: "napi_".padEnd(64, "b"),
      organizationId: null,
    })).rejects.toMatchObject({
      provider: "neon",
      status: 424,
    });
  });
});
