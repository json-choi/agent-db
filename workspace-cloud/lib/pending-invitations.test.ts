import { describe, expect, it, vi } from "vitest";
import { acceptPendingWorkspaceInvitations } from "./pending-invitations";

function invitation(id: string, expiresAt = "2026-07-24T00:00:00.000Z") {
  return { id, status: "pending", expiresAt };
}

describe("acceptPendingWorkspaceInvitations", () => {
  it("does not enumerate invitations for an unverified email", async () => {
    const listUserInvitations = vi.fn();
    const result = await acceptPendingWorkspaceInvitations({
      api: {
        listUserInvitations,
        acceptInvitation: vi.fn(),
        setActiveOrganization: vi.fn(),
      },
      headers: new Headers(),
      user: { email: "invited@example.com", emailVerified: false },
    });

    expect(result).toEqual({ accepted: 0, failed: 0 });
    expect(listUserInvitations).not.toHaveBeenCalled();
  });

  it("accepts every live invitation for the normalized verified email", async () => {
    const listUserInvitations = vi.fn().mockResolvedValue([
      invitation("invite-a"),
      invitation("invite-b"),
      invitation("invite-b"),
      invitation("expired", "2026-07-22T00:00:00.000Z"),
      { ...invitation("cancelled"), status: "canceled" },
    ]);
    const acceptInvitation = vi.fn().mockResolvedValue({});

    const result = await acceptPendingWorkspaceInvitations({
      api: {
        listUserInvitations,
        acceptInvitation,
        setActiveOrganization: vi.fn(),
      },
      headers: new Headers({ authorization: "Bearer session-token" }),
      user: { email: "  Invited@Example.COM ", emailVerified: true },
      now: new Date("2026-07-23T00:00:00.000Z"),
    });

    expect(listUserInvitations).toHaveBeenCalledWith({
      query: { email: "invited@example.com" },
    });
    expect(acceptInvitation).toHaveBeenCalledTimes(2);
    expect(acceptInvitation).toHaveBeenNthCalledWith(1, {
      body: { invitationId: "invite-a" },
      headers: expect.any(Headers),
    });
    expect(acceptInvitation).toHaveBeenNthCalledWith(2, {
      body: { invitationId: "invite-b" },
      headers: expect.any(Headers),
    });
    expect(result).toEqual({ accepted: 2, failed: 0 });
  });

  it("keeps accepting other invitations and restores the active workspace", async () => {
    const acceptInvitation = vi.fn()
      .mockRejectedValueOnce(new Error("already accepted"))
      .mockResolvedValueOnce({});
    const setActiveOrganization = vi.fn().mockResolvedValue({});

    const result = await acceptPendingWorkspaceInvitations({
      api: {
        listUserInvitations: vi.fn().mockResolvedValue([
          invitation("raced"),
          invitation("accepted"),
        ]),
        acceptInvitation,
        setActiveOrganization,
      },
      headers: new Headers(),
      user: { email: "invited@example.com", emailVerified: true },
      activeOrganizationId: "existing-workspace",
      now: new Date("2026-07-23T00:00:00.000Z"),
    });

    expect(result).toEqual({ accepted: 1, failed: 1 });
    expect(setActiveOrganization).toHaveBeenCalledWith({
      body: { organizationId: "existing-workspace" },
      headers: expect.any(Headers),
    });
  });
});
