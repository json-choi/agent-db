import { describe, expect, it } from "vitest";
import { isTransientDbError } from "./queries";

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
