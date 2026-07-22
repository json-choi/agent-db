"use client";

import { createAuthClient } from "better-auth/react";
import {
  deviceAuthorizationClient,
  multiSessionClient,
  organizationClient,
} from "better-auth/client/plugins";
import { ac, workspaceRoles } from "./access";

export const authClient = createAuthClient({
  plugins: [
    multiSessionClient(),
    deviceAuthorizationClient(),
    organizationClient({ ac, roles: workspaceRoles }),
  ],
});
