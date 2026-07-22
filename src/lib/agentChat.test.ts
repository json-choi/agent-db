import { describe, expect, it } from "vitest";
import type { ChatThread } from "../ipc/types";
import { connectionThreads } from "./agentChat";

function thread(id: string, connectionId: string | null): ChatThread {
  return {
    id,
    provider: "claude",
    connectionId,
    title: id,
    cliSessionId: null,
    model: null,
    effort: null,
    createdAt: "2026-07-23T00:00:00.000Z",
    updatedAt: "2026-07-23T00:00:00.000Z",
  };
}

describe("connectionThreads", () => {
  it("returns only conversations bound to the selected database", () => {
    const threads = [thread("a-1", "db-a"), thread("b-1", "db-b"), thread("a-2", "db-a")];

    expect(connectionThreads(threads, "db-a").map((item) => item.id)).toEqual([
      "a-1",
      "a-2",
    ]);
  });

  it("does not expose legacy unscoped conversations", () => {
    expect(connectionThreads([thread("legacy", null)], "db-a")).toEqual([]);
  });
});
