import { describe, expect, it } from "vitest";
import {
  isSafeDisplayText,
  isUuid,
  mutationAllowed,
  privateJson,
  safeReturnTo,
  singleLineText,
} from "./http";

describe("workspace HTTP boundaries", () => {
  it("marks identity-scoped JSON as private and non-cacheable", async () => {
    const response = privateJson({ ok: true }, { status: 201 });

    expect(response.status).toBe(201);
    expect(response.headers.get("cache-control")).toBe("private, no-store");
    await expect(response.json()).resolves.toEqual({ ok: true });
  });

  it("allows same-origin browser mutations and bearer desktop calls only", () => {
    const origin = "https://app.dopedb.dev";

    expect(mutationAllowed(new Request(`${origin}/api`, {
      headers: { origin },
    }), origin)).toBe(true);
    expect(mutationAllowed(new Request(`${origin}/api`, {
      headers: { authorization: "Bearer opaque-session" },
    }), origin)).toBe(true);
    expect(mutationAllowed(new Request(`${origin}/api`, {
      headers: { origin: "https://attacker.example" },
    }), origin)).toBe(false);
  });

  it("rejects protocol-relative, backslash, and external return targets", () => {
    expect(safeReturnTo("/settings?workspace=1")).toBe("/settings?workspace=1");
    expect(safeReturnTo("//attacker.example/path")).toBe("/settings");
    expect(safeReturnTo("/\\attacker.example/path")).toBe("/settings");
    expect(safeReturnTo("/%5c%5cattacker.example/path")).toBe("/settings");
    expect(safeReturnTo("https://attacker.example/path")).toBe("/settings");
  });

  it("accepts canonical UUIDs and rejects values PostgreSQL cannot cast", () => {
    expect(isUuid("019bf6c8-2d35-7ba1-89bf-b4698600478c")).toBe(true);
    expect(isUuid("../../connection")).toBe(false);
    expect(isUuid("00000000-0000-0000-0000-000000000000")).toBe(false);
  });

  it("keeps display names single-line at UI and email boundaries", () => {
    expect(isSafeDisplayText("제품 데이터 팀", 120)).toBe(true);
    expect(isSafeDisplayText("spoofed\nsubject", 120)).toBe(false);
    expect(singleLineText("  제품\n\t데이터 팀  ")).toBe("제품 데이터 팀");
  });
});
