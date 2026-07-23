// Better Auth owns identity, sessions, invitations, and organization records.
// Membership authority mutations are narrowed to the revocation-gated workspace routes.
// Provider credentials are stripped before persistence; desktop sessions use RFC 8628.
import "server-only";
import { randomUUID } from "node:crypto";
import { betterAuth } from "better-auth";
import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import { bearer, deviceAuthorization, multiSession, organization } from "better-auth/plugins";
import { db } from "./db";
import { env } from "./env";
import { sendWorkspaceInvitation } from "./invitation-email";
import { authSchema, workspaceAuditEvent, workspaceProfile } from "./schema";
import { ac, workspaceRoles } from "./access";

function withoutProviderTokens<T extends Record<string, unknown>>(account: T): T {
  return {
    ...account,
    accessToken: null,
    refreshToken: null,
    idToken: null,
    accessTokenExpiresAt: null,
    refreshTokenExpiresAt: null,
  };
}

export const auth = betterAuth({
  appName: "DopeDB Workspace",
  baseURL: env.appOrigin(),
  secret: env.authSecret(),
  trustedOrigins: [env.appOrigin()],
  // Membership authority changes must pass through the workspace routes that
  // durably block lease issuance and drain existing credentials first. Better
  // Auth's server-side auth.api methods remain available to those routes.
  disabledPaths: [
    "/organization/update-member-role",
    "/organization/remove-member",
    "/organization/leave",
  ],
  database: drizzleAdapter(db, {
    provider: "pg",
    schema: authSchema,
  }),
  socialProviders: {
    google: {
      clientId: env.googleClientId(),
      clientSecret: env.googleClientSecret(),
      prompt: "select_account",
    },
  },
  account: {
    updateAccountOnSignIn: false,
    storeAccountCookie: false,
  },
  session: {
    expiresIn: 60 * 60 * 24 * 30,
    updateAge: 60 * 60 * 24,
    cookieCache: { enabled: true, maxAge: 60 * 5 },
  },
  rateLimit: {
    enabled: true,
    storage: "database",
    window: 60,
    max: 100,
  },
  advanced: {
    database: { generateId: "uuid" },
    useSecureCookies: process.env.NODE_ENV === "production",
  },
  databaseHooks: {
    account: {
      create: { before: async (account) => ({ data: withoutProviderTokens(account) }) },
      update: { before: async (account) => ({ data: withoutProviderTokens(account) }) },
    },
  },
  plugins: [
    // Keep several browser identities available without merging their organization
    // boundaries. The active session remains explicit and can be switched atomically.
    multiSession({ maximumSessions: 10 }),
    // RFC 8628 returns the Better Auth session token directly; the desktop stores it
    // in the OS credential store and presents it only over HTTPS as a Bearer token.
    bearer(),
    deviceAuthorization({
      verificationUri: "/auth/device",
      expiresIn: "10m",
      interval: "5s",
      validateClient: async (clientId) => clientId === "dopedb-desktop",
    }),
    organization({
      ac,
      roles: workspaceRoles,
      creatorRole: "owner",
      disableOrganizationDeletion: true,
      membershipLimit: 100,
      invitationExpiresIn: 60 * 60 * 48,
      cancelPendingInvitationsOnReInvite: true,
      requireEmailVerificationOnInvitation: true,
      sendInvitationEmail: async (data) => {
        await sendWorkspaceInvitation(data, env.appOrigin());
      },
      organizationHooks: {
        afterCreateOrganization: async ({ organization, user }) => {
          await db
            .insert(workspaceProfile)
            .values({
              organizationId: organization.id,
              encryptionKeyRef: `pending://${organization.id}`,
              residencyRegion: process.env.VERCEL_REGION ?? null,
            })
            .onConflictDoNothing();
          await db.insert(workspaceAuditEvent).values({
            organizationId: organization.id,
            actorUserId: user.id,
            action: "workspace.create",
            resourceType: "workspace",
            resourceId: organization.id,
            redactedSummary: { name: organization.name },
            requestId: randomUUID(),
          });
        },
      },
    }),
  ],
});
