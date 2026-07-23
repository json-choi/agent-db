// Drizzle schema for Better Auth and workspace collaboration metadata. Shared
// connection columns intentionally cannot represent target-database credentials.
import {
  bigint,
  boolean,
  index,
  integer,
  jsonb,
  pgSchema,
  text,
  timestamp,
  uniqueIndex,
  uuid,
} from "drizzle-orm/pg-core";
import { sql } from "drizzle-orm";

export const workspaceControl = pgSchema("workspace_control");

export const user = workspaceControl.table("user", {
  id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
  name: text("name").notNull(),
  email: text("email").notNull().unique(),
  emailVerified: boolean("email_verified").notNull().default(false),
  image: text("image"),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
  updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
});

export const organization = workspaceControl.table("organization", {
  id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
  name: text("name").notNull(),
  slug: text("slug").notNull().unique(),
  logo: text("logo"),
  metadata: text("metadata"),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
});

export const session = workspaceControl.table(
  "session",
  {
    id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
    expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
    token: text("token").notNull().unique(),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
    ipAddress: text("ip_address"),
    userAgent: text("user_agent"),
    userId: text("user_id").notNull().references(() => user.id, { onDelete: "cascade" }),
    activeOrganizationId: text("active_organization_id").references(() => organization.id, {
      onDelete: "set null",
    }),
  },
  (table) => [index("session_user_idx").on(table.userId)],
);

export const account = workspaceControl.table(
  "account",
  {
    id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
    accountId: text("account_id").notNull(),
    providerId: text("provider_id").notNull(),
    userId: text("user_id").notNull().references(() => user.id, { onDelete: "cascade" }),
    accessToken: text("access_token"),
    refreshToken: text("refresh_token"),
    idToken: text("id_token"),
    accessTokenExpiresAt: timestamp("access_token_expires_at", { withTimezone: true }),
    refreshTokenExpiresAt: timestamp("refresh_token_expires_at", { withTimezone: true }),
    scope: text("scope"),
    password: text("password"),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [
    index("account_user_idx").on(table.userId),
    uniqueIndex("account_provider_subject_idx").on(table.providerId, table.accountId),
  ],
);

export const verification = workspaceControl.table(
  "verification",
  {
    id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
    identifier: text("identifier").notNull(),
    value: text("value").notNull(),
    expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [index("verification_identifier_idx").on(table.identifier)],
);

export const member = workspaceControl.table(
  "member",
  {
    id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
    organizationId: text("organization_id").notNull().references(() => organization.id, {
      onDelete: "cascade",
    }),
    userId: text("user_id").notNull().references(() => user.id, { onDelete: "cascade" }),
    role: text("role").notNull().default("viewer"),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [
    uniqueIndex("member_organization_user_idx").on(table.organizationId, table.userId),
    index("member_user_idx").on(table.userId),
  ],
);

export const invitation = workspaceControl.table(
  "invitation",
  {
    id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
    organizationId: text("organization_id").notNull().references(() => organization.id, {
      onDelete: "cascade",
    }),
    email: text("email").notNull(),
    role: text("role"),
    status: text("status").notNull().default("pending"),
    expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    inviterId: text("inviter_id").notNull().references(() => user.id, { onDelete: "cascade" }),
  },
  (table) => [
    index("invitation_organization_idx").on(table.organizationId),
    index("invitation_email_idx").on(table.email),
  ],
);

export const deviceCode = workspaceControl.table(
  "device_code",
  {
    id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
    deviceCode: text("device_code").notNull().unique(),
    userCode: text("user_code").notNull().unique(),
    userId: text("user_id").references(() => user.id, { onDelete: "cascade" }),
    expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
    status: text("status").notNull(),
    lastPolledAt: timestamp("last_polled_at", { withTimezone: true }),
    pollingInterval: integer("polling_interval"),
    clientId: text("client_id"),
    scope: text("scope"),
  },
  (table) => [index("device_code_user_idx").on(table.userId)],
);

export const rateLimit = workspaceControl.table("rate_limit", {
  id: text("id").default(sql`gen_random_uuid()::text`).primaryKey(),
  key: text("key").notNull().unique(),
  count: integer("count").notNull(),
  lastRequest: bigint("last_request", { mode: "number" }).notNull(),
});

export const workspaceProfile = workspaceControl.table("workspace_profile", {
  organizationId: text("organization_id").primaryKey().references(() => organization.id, {
    onDelete: "cascade",
  }),
  lifecycleState: text("lifecycle_state").notNull().default("active"),
  encryptionKeyRef: text("encryption_key_ref").notNull(),
  residencyRegion: text("residency_region"),
  revision: bigint("revision", { mode: "number" }).notNull().default(1),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
  updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
});

export const workspaceAuditEvent = workspaceControl.table(
  "workspace_audit_event",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: text("organization_id").notNull().references(() => organization.id, {
      onDelete: "cascade",
    }),
    actorUserId: text("actor_user_id").references(() => user.id, { onDelete: "set null" }),
    action: text("action").notNull(),
    resourceType: text("resource_type").notNull(),
    resourceId: text("resource_id"),
    redactedSummary: jsonb("redacted_summary").notNull().default({}),
    requestId: uuid("request_id").notNull(),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [index("workspace_audit_org_created_idx").on(table.organizationId, table.createdAt)],
);

// Long-lived provider authorization is isolated from connection templates. The
// credential payload is application-encrypted before it reaches this column; public
// serializers never select it.
export const workspaceProviderIntegration = workspaceControl.table(
  "workspace_provider_integration",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: text("organization_id").notNull().references(() => organization.id, {
      onDelete: "cascade",
    }),
    provider: text("provider").notNull(),
    status: text("status").notNull().default("active"),
    externalAccountId: text("external_account_id").notNull(),
    displayName: text("display_name").notNull(),
    encryptedCredential: text("encrypted_credential").notNull(),
    credentialExpiresAt: timestamp("credential_expires_at", { withTimezone: true }),
    grantedScope: text("granted_scope"),
    createdByUserId: text("created_by_user_id").references(() => user.id, {
      onDelete: "set null",
    }),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
    revokedAt: timestamp("revoked_at", { withTimezone: true }),
  },
  (table) => [
    uniqueIndex("provider_integration_org_provider_account_idx").on(
      table.organizationId,
      table.provider,
      table.externalAccountId,
    ),
    index("provider_integration_org_status_idx").on(
      table.organizationId,
      table.status,
    ),
  ],
);

// OAuth state is single-use server data rather than a browser-readable cookie. Only
// a SHA-256 digest is retained, limiting the value of a database disclosure.
export const providerOauthState = workspaceControl.table(
  "provider_oauth_state",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: text("organization_id").notNull().references(() => organization.id, {
      onDelete: "cascade",
    }),
    userId: text("user_id").notNull().references(() => user.id, {
      onDelete: "cascade",
    }),
    provider: text("provider").notNull(),
    stateHash: text("state_hash").notNull(),
    expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [
    uniqueIndex("provider_oauth_state_hash_idx").on(table.stateHash),
    index("provider_oauth_state_expiry_idx").on(table.expiresAt),
  ],
);

// Shared connection rows are deliberately templates, not credentials. There is no
// username, password, token, certificate, connection URL, or local secret reference
// column in this table, so those values cannot be uploaded accidentally by the API.
export const workspaceConnection = workspaceControl.table(
  "workspace_connection",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: text("organization_id").notNull().references(() => organization.id, {
      onDelete: "cascade",
    }),
    name: text("name").notNull(),
    engine: text("engine").notNull(),
    provider: text("provider").notNull().default("auto"),
    driverId: text("driver_id"),
    host: text("host").notNull(),
    port: integer("port").notNull(),
    databaseName: text("database_name").notNull(),
    sslmode: text("sslmode").notNull(),
    readonlyDefault: boolean("readonly_default").notNull().default(true),
    allowWrites: boolean("allow_writes").notNull().default(false),
    credentialMode: text("credential_mode").notNull().default("member_local"),
    providerIntegrationId: uuid("provider_integration_id").references(
      () => workspaceProviderIntegration.id,
      { onDelete: "set null" },
    ),
    providerResource: jsonb("provider_resource"),
    environment: text("environment"),
    schemaGroup: text("schema_group"),
    revision: bigint("revision", { mode: "number" }).notNull().default(1),
    createdByUserId: text("created_by_user_id").references(() => user.id, {
      onDelete: "set null",
    }),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    updatedAt: timestamp("updated_at", { withTimezone: true }).notNull().defaultNow(),
    deletedAt: timestamp("deleted_at", { withTimezone: true }),
  },
  (table) => [
    index("workspace_connection_org_updated_idx").on(
      table.organizationId,
      table.updatedAt,
    ),
  ],
);

// Lease rows are a secret-free revocation and audit index. One-time passwords and
// tokens are returned directly to the native client and are never inserted here.
export const workspaceCredentialLease = workspaceControl.table(
  "workspace_credential_lease",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: text("organization_id").notNull().references(() => organization.id, {
      onDelete: "cascade",
    }),
    connectionId: uuid("connection_id").notNull().references(() => workspaceConnection.id, {
      onDelete: "cascade",
    }),
    integrationId: uuid("integration_id").notNull().references(
      () => workspaceProviderIntegration.id,
      { onDelete: "cascade" },
    ),
    userId: text("user_id").notNull().references(() => user.id, {
      onDelete: "cascade",
    }),
    provider: text("provider").notNull(),
    accessMode: text("access_mode").notNull(),
    externalCredentialId: text("external_credential_id").notNull(),
    externalCredentialKind: text("external_credential_kind").notNull(),
    expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
    revokedAt: timestamp("revoked_at", { withTimezone: true }),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [
    index("credential_lease_member_active_idx").on(
      table.organizationId,
      table.userId,
      table.expiresAt,
    ),
    index("credential_lease_connection_active_idx").on(
      table.connectionId,
      table.expiresAt,
    ),
  ],
);

export const authSchema = {
  user,
  session,
  account,
  verification,
  organization,
  member,
  invitation,
  deviceCode,
  rateLimit,
};
