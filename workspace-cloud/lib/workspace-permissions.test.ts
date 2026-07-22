import { describe, expect, it } from "vitest";
import { accessModeForRole, hasWorkspaceCapability } from "./workspace-permissions";

describe("workspace role capabilities", () => {
  it("requires Admin or Owner for connection-template management", () => {
    expect(hasWorkspaceCapability("owner", "manage")).toBe(true);
    expect(hasWorkspaceCapability("admin", "manage")).toBe(true);
    expect(hasWorkspaceCapability("editor", "manage")).toBe(false);
  });

  it("maps roles to their strongest execution access", () => {
    expect(accessModeForRole("viewer")).toBe("view");
    expect(accessModeForRole("analyst")).toBe("read");
    expect(accessModeForRole("editor")).toBe("write");
    expect(accessModeForRole("admin")).toBe("manage");
  });

  it("reserves workspace deletion for the owner", () => {
    expect(hasWorkspaceCapability("owner", "delete")).toBe(true);
    expect(hasWorkspaceCapability("admin", "delete")).toBe(false);
  });
});
