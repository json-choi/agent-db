import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { WorkspaceAuthState } from "../ipc/types";
import { resetWorkspaceResourceQueries } from "./queryClient";
import { qk } from "./queries";

describe("workspace query lifecycle", () => {
  it("clears workspace data without dropping signed-in identity or global data", async () => {
    const client = new QueryClient();
    const auth: WorkspaceAuthState = {
      authenticated: true,
      user: { id: "user-1", email: "user@example.com", displayName: "User" },
    };
    client.setQueryData(qk.workspaceAuth(), auth);
    client.setQueryData(qk.catalog("connection-1"), { tables: [] });
    client.setQueryData(qk.drivers(), [{ id: "bundled" }]);

    await resetWorkspaceResourceQueries(client);

    expect(client.getQueryData(qk.workspaceAuth())).toEqual(auth);
    expect(client.getQueryData(qk.catalog("connection-1"))).toBeUndefined();
    expect(client.getQueryData(qk.drivers())).toEqual([{ id: "bundled" }]);
  });
});
