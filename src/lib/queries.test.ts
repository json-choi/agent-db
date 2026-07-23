import { describe, expect, it } from "vitest";
import { isTransientDbError, mcpPlatformsQuery, qk } from "./queries";

describe("isTransientDbError", () => {
  it("treats network-shaped failures as transient", () => {
    expect(isTransientDbError("database error: pool timed out while waiting for an open connection")).toBe(true);
    expect(isTransientDbError("Schema loading timed out. Check the database connection or retry.")).toBe(true);
    expect(isTransientDbError(new Error("connection refused"))).toBe(true);
    expect(isTransientDbError("host unreachable")).toBe(true);
  });

  it("keeps deterministic failures failing fast", () => {
    expect(isTransientDbError("password authentication failed for user")).toBe(false);
    expect(isTransientDbError('relation "users" does not exist')).toBe(false);
    expect(isTransientDbError("permission denied for table accounts")).toBe(false);
  });
});

describe("MCP platform query lifecycle", () => {
  it("uses one global key and keeps a warm result between screen mounts", () => {
    const query = mcpPlatformsQuery();

    expect(query.queryKey).toEqual(qk.mcpPlatforms());
    expect(query.staleTime).toBe(5 * 60_000);
  });
});
