import { createAccessControl } from "better-auth/plugins/access";
import {
  adminAc,
  defaultStatements,
  memberAc,
  ownerAc,
} from "better-auth/plugins/organization/access";

const statement = {
  ...defaultStatements,
  workspace: ["view", "execute", "edit", "manage", "delete"],
} as const;

export const ac = createAccessControl(statement);
const viewer = ac.newRole({ ...memberAc.statements, workspace: ["view"] });
const analyst = ac.newRole({ ...memberAc.statements, workspace: ["view", "execute"] });
const editor = ac.newRole({
  ...memberAc.statements,
  workspace: ["view", "execute", "edit"],
});
const admin = ac.newRole({
  ...adminAc.statements,
  workspace: ["view", "execute", "edit", "manage"],
});
const owner = ac.newRole({
  ...ownerAc.statements,
  workspace: ["view", "execute", "edit", "manage", "delete"],
});

export const workspaceRoles = { viewer, analyst, editor, admin, owner };
