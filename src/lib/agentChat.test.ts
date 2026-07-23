import { describe, expect, it } from "vitest";
import type { ChatMessageRecord, ChatThread } from "../ipc/types";
import {
  connectionThreads,
  shouldShowOptimisticUser,
  shouldShowStreamingAssistant,
  type PendingTurn,
} from "./agentChat";

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

function message(id: string, role: "user" | "assistant"): ChatMessageRecord {
  return {
    id,
    threadId: "thread-1",
    role,
    text: "same prompt",
    error: null,
    createdAt: "2026-07-23T00:00:00.000Z",
  };
}

const pending: PendingTurn = {
  threadId: "thread-1",
  turnId: "turn-1",
  userMessageId: "user-1",
  userText: "same prompt",
  assistantText: "",
  turnStartIso: "2026-07-23T00:00:00.000Z",
  done: false,
};

describe("shouldShowOptimisticUser", () => {
  it("hides the optimistic echo after its durable row is loaded", () => {
    expect(shouldShowOptimisticUser([message("user-1", "user")], pending)).toBe(false);
  });

  it("keeps the echo when an identical earlier prompt has a different id", () => {
    expect(shouldShowOptimisticUser([message("user-0", "user")], pending)).toBe(true);
  });
});

describe("shouldShowStreamingAssistant", () => {
  it("hides the streaming echo after the durable response is loaded", () => {
    expect(shouldShowStreamingAssistant([message("turn-1", "assistant")], pending)).toBe(
      false,
    );
  });

  it("keeps the streaming response until its own durable row is loaded", () => {
    expect(shouldShowStreamingAssistant([message("turn-0", "assistant")], pending)).toBe(
      true,
    );
  });
});
