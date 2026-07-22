import { describe, expect, it } from "vitest";
import { sessionsExceptCurrent } from "./device-session-policy";

describe("device session logout policy", () => {
  it("revokes other sessions even when they belong to the current user", () => {
    const sessions = [
      { session: { id: "current" }, userId: "user-a" },
      { session: { id: "same-user" }, userId: "user-a" },
      { session: { id: "other-user" }, userId: "user-b" },
    ];

    expect(sessionsExceptCurrent(sessions, "current").map((item) => item.session.id))
      .toEqual(["same-user", "other-user"]);
  });

  it("revokes every listed session when the active session is not in the list", () => {
    const sessions = [{ session: { id: "other" } }];

    expect(sessionsExceptCurrent(sessions, "missing")).toEqual(sessions);
  });
});
