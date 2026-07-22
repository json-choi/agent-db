import { describe, expect, it } from "vitest";
import {
  shouldRevalidateWorkspaceAuth,
  WORKSPACE_AUTH_RECHECK_MS,
} from "./workspaceAuthLifecycle";

describe("workspace auth lifecycle", () => {
  it("keeps a recently verified signed-in state stable", () => {
    expect(shouldRevalidateWorkspaceAuth(true, 1_000, false, 1_000 + 60_000)).toBe(false);
  });

  it("revalidates a signed-in state after the cooldown", () => {
    expect(
      shouldRevalidateWorkspaceAuth(
        true,
        1_000,
        false,
        1_000 + WORKSPACE_AUTH_RECHECK_MS,
      ),
    ).toBe(true);
  });

  it("does not duplicate an in-flight check or poll anonymous state", () => {
    expect(shouldRevalidateWorkspaceAuth(true, 0, true, WORKSPACE_AUTH_RECHECK_MS)).toBe(false);
    expect(shouldRevalidateWorkspaceAuth(false, 0, false, WORKSPACE_AUTH_RECHECK_MS)).toBe(false);
  });
});
