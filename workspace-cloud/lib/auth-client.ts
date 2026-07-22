"use client";

import { createAuthClient } from "better-auth/react";
import { deviceAuthorizationClient, organizationClient } from "better-auth/client/plugins";
import { ac, workspaceRoles } from "./access";

export const authClient = createAuthClient({
  plugins: [
    deviceAuthorizationClient(),
    organizationClient({ ac, roles: workspaceRoles }),
  ],
});
