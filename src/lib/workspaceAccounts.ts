// Pure account/workspace projection used by the desktop switcher and secure-copy
// dialog. Composite option values keep duplicate cross-account memberships distinct.
import type {
  Workspace,
  WorkspaceAuthState,
  WorkspaceRole,
} from "../ipc/types";

interface WorkspaceChoice {
  value: string;
  workspace: Workspace;
  accountUserId: string | null;
  role: WorkspaceRole | null;
}

export interface WorkspaceChoiceGroup {
  key: string;
  label: string;
  choices: WorkspaceChoice[];
}

const LOCAL_ACCOUNT = "local";

export function canManageWorkspaceConnections(role: WorkspaceRole | null) {
  return role === "admin" || role === "owner";
}

export function workspaceChoiceValue(workspaceId: string, accountUserId: string | null) {
  return `${accountUserId ?? LOCAL_ACCOUNT}:${workspaceId}`;
}

export function parseWorkspaceChoice(value: string) {
  const separator = value.indexOf(":");
  if (separator < 1 || separator === value.length - 1) return null;
  const account = value.slice(0, separator);
  return {
    workspaceId: value.slice(separator + 1),
    accountUserId: account === LOCAL_ACCOUNT ? null : account,
  };
}

export function buildWorkspaceChoiceGroups(
  auth: WorkspaceAuthState | undefined,
  workspaces: Workspace[],
  localLabel: string,
): WorkspaceChoiceGroup[] {
  const byId = new Map(workspaces.map((workspace) => [workspace.id, workspace]));
  const personal = workspaces.find((workspace) => workspace.kind === "personal");
  const groups: WorkspaceChoiceGroup[] = personal
    ? [{
        key: LOCAL_ACCOUNT,
        label: localLabel,
        choices: [{
          value: workspaceChoiceValue(personal.id, null),
          workspace: personal,
          accountUserId: null,
          role: null,
        }],
      }]
    : [];

  for (const account of auth?.accounts ?? []) {
    const choices = account.memberships.flatMap((membership) => {
      const workspace = byId.get(membership.workspaceId);
      return workspace?.kind === "team"
        ? [{
            value: workspaceChoiceValue(workspace.id, account.user.id),
            workspace,
            accountUserId: account.user.id,
            role: membership.role,
          }]
        : [];
    });
    if (choices.length > 0) {
      groups.push({
        key: account.user.id,
        label: account.user.email,
        choices,
      });
    }
  }
  return groups;
}
