import { describe, expect, it, vi } from "vitest";
import { cancelTracked, runTracked } from "./useQueryRun";

const { cancelQuery } = vi.hoisted(() => ({
  cancelQuery: vi.fn(async (_queryId: string) => true),
}));
vi.mock("../ipc/commands", () => ({ cancelQuery }));

describe("runTracked / cancelTracked", () => {
  it("resolves with the value on success", async () => {
    const tracker = { queryId: null, cancelled: false };
    const outcome = await runTracked(tracker, async () => 42);
    expect(outcome).toEqual({ cancelled: false, value: 42 });
    expect(tracker.queryId).toBeNull();
  });

  it("rethrows a real error when the run was never cancelled", async () => {
    const tracker = { queryId: null, cancelled: false };
    const boom = new Error("boom");
    await expect(runTracked(tracker, async () => Promise.reject(boom))).rejects.toBe(boom);
  });

  it("swallows the rejection and calls cancelQuery when the run was cancelled first", async () => {
    cancelQuery.mockClear();
    const tracker = { queryId: null, cancelled: false };
    const outcome = await runTracked(tracker, async (queryId) => {
      // Simulate the backend rejecting an in-flight query once it's been cancelled.
      cancelTracked(tracker);
      expect(tracker.queryId).toBe(queryId);
      throw new Error("cancelled by user");
    });

    expect(outcome).toEqual({ cancelled: true });
    expect(cancelQuery).toHaveBeenCalledTimes(1);
  });

  it("cancelTracked is a no-op when nothing is running", () => {
    cancelQuery.mockClear();
    const tracker = { queryId: null, cancelled: false };
    cancelTracked(tracker);
    expect(cancelQuery).not.toHaveBeenCalled();
    expect(tracker.cancelled).toBe(false);
  });
});
