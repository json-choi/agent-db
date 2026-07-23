// Pure Neon validation, identity, and SQL construction. Control-plane API roles
// inherit neon_superuser, so managed leases create constrained SQL roles instead.

import {
  createHash,
  createHmac,
  pbkdf2Sync,
  randomBytes,
} from "node:crypto";
import type { ManagedAccessMode } from "./provider-types";

export const NEON_LEASE_SECONDS = 15 * 60;
export const NEON_ROLE_CONNECTION_LIMIT = 4;
export const NEON_PUBLIC_DATABASE_ESCAPE_SQL =
  "SELECT acl.privilege_type AS privilege_type FROM pg_database d "
  + "CROSS JOIN LATERAL aclexplode("
  + "COALESCE(d.datacl, acldefault('d', d.datdba))) acl "
  + "WHERE d.datname = current_database() "
  + "AND acl.grantee = 0 "
  + "AND acl.privilege_type = ANY("
  + "ARRAY['CREATE', 'TEMPORARY']::text[]) "
  + "ORDER BY acl.privilege_type";
export const NEON_PUBLIC_SCHEMA_ESCAPE_SQL =
  "SELECT n.nspname AS schema_name FROM pg_namespace n "
  + "CROSS JOIN LATERAL aclexplode("
  + "COALESCE(n.nspacl, acldefault('n', n.nspowner))) acl "
  + "WHERE lower(n.nspname) <> 'information_schema' "
  + "AND lower(n.nspname) !~ '^pg_' "
  + "AND NOT (n.nspname = ANY($1::text[])) "
  + "AND acl.grantee = 0 "
  + "AND acl.privilege_type = ANY(ARRAY['USAGE', 'CREATE']::text[]) LIMIT 1";
export const NEON_PUBLIC_SCHEMA_CREATE_SQL =
  "SELECT n.nspname AS schema_name FROM pg_namespace n "
  + "CROSS JOIN LATERAL aclexplode("
  + "COALESCE(n.nspacl, acldefault('n', n.nspowner))) acl "
  + "WHERE n.nspname = ANY($1::text[]) "
  + "AND acl.grantee = 0 AND acl.privilege_type = 'CREATE' LIMIT 1";
const SCRAM_ITERATIONS = 4_096;
const SCRAM_SALT_BYTES = 16;
const DEFAULT_SCHEMAS = ["public"] as const;

export function neonPublicDatabaseBoundaryError(
  privileges: unknown[],
): string | null {
  const normalized = new Set(
    privileges
      .filter((privilege): privilege is string => typeof privilege === "string")
      .map((privilege) => privilege.toUpperCase()),
  );
  const create = normalized.has("CREATE");
  const temporary = normalized.has("TEMPORARY") || normalized.has("TEMP");
  if (create && temporary) {
    return "Neon PUBLIC database CREATE and TEMPORARY privileges permit "
      + "unscoped object writes; revoke both privileges from PUBLIC";
  }
  if (create) {
    return "Neon PUBLIC database CREATE privilege escapes the managed schema "
      + "allowlist; revoke CREATE from PUBLIC";
  }
  if (temporary) {
    return "Neon PUBLIC database TEMPORARY privilege permits unscoped temporary "
      + "writes; revoke TEMPORARY from PUBLIC";
  }
  return null;
}

export type NeonCredential = {
  apiKey: string;
  organizationId: string | null;
};

export type NeonResource = {
  project: string;
  branch: string;
  database: string;
  engine: "postgres";
  schemas: string[];
};

export type NeonIdentitySubject = {
  kind: "user" | "organization";
  id: string;
};

export function neonSegment(value: unknown): value is string {
  return typeof value === "string"
    && /^[a-z0-9][a-z0-9-]{0,59}$/.test(value);
}

export function neonDatabaseName(value: unknown): value is string {
  return typeof value === "string"
    && value.length > 0
    && value.length <= 63
    && !/[\u0000-\u001f\u007f/?#]/.test(value);
}

export function neonSchemaName(value: unknown): value is string {
  if (
    typeof value !== "string"
    || value.length === 0
    || value.length > 63
    || /[\u0000-\u001f\u007f]/.test(value)
  ) {
    return false;
  }
  const normalized = value.toLowerCase();
  return normalized !== "information_schema"
    && normalized !== "neon"
    && normalized !== "neon_auth"
    && !normalized.startsWith("pg_");
}

function neonSchemas(value: unknown): string[] {
  const schemas = value === undefined ? [...DEFAULT_SCHEMAS] : value;
  if (
    !Array.isArray(schemas)
    || schemas.length === 0
    || schemas.length > 32
    || schemas.some((schema) => !neonSchemaName(schema))
    || new Set(schemas).size !== schemas.length
  ) {
    throw new Error("Invalid Neon schema allowlist");
  }
  return [...schemas];
}

export function parseNeonResource(value: unknown): NeonResource {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Neon resource is required");
  }
  const body = value as Record<string, unknown>;
  if (
    !neonSegment(body.project)
    || !neonSegment(body.branch)
    || !neonDatabaseName(body.database)
    || body.engine !== "postgres"
  ) {
    throw new Error("Invalid Neon resource");
  }
  return {
    project: body.project,
    branch: body.branch,
    database: body.database,
    engine: "postgres",
    schemas: neonSchemas(body.schemas),
  };
}

export function neonScopeFingerprint(projectIds: string[]): string {
  if (
    projectIds.length === 0
    || projectIds.length > 4_000
    || projectIds.some((project) => !neonSegment(project))
  ) {
    throw new Error("Invalid Neon project scope");
  }
  const projects = [...new Set(projectIds)].sort();
  if (projects.length !== projectIds.length) {
    throw new Error("Invalid Neon project scope");
  }
  return createHash("sha256")
    .update(projects.join("\n"), "utf8")
    .digest("base64url");
}

export function neonIntegrationIdentity(
  subject: NeonIdentitySubject,
  projectIds: string[],
): { externalAccountId: string; scopeFingerprint: string } {
  if (
    !["user", "organization"].includes(subject.kind)
    || !subject.id
    || subject.id.length > 2_048
    || /[\u0000-\u001f\u007f]/.test(subject.id)
  ) {
    throw new Error("Invalid Neon identity subject");
  }
  const subjectFingerprint = createHash("sha256")
    .update(`${subject.kind}:${subject.id}`, "utf8")
    .digest("base64url");
  const scopeFingerprint = neonScopeFingerprint(projectIds);
  return {
    externalAccountId: `neon:v2:${subject.kind}:${subjectFingerprint}:${scopeFingerprint}`,
    scopeFingerprint,
  };
}

export function createNeonScramVerifier(
  password: string,
  salt: Buffer = randomBytes(SCRAM_SALT_BYTES),
): string {
  if (!/^[A-Za-z0-9_-]{43}$/.test(password) || salt.length !== SCRAM_SALT_BYTES) {
    throw new Error("Invalid Neon role material");
  }
  const saltedPassword = pbkdf2Sync(
    password,
    salt,
    SCRAM_ITERATIONS,
    32,
    "sha256",
  );
  const clientKey = createHmac("sha256", saltedPassword)
    .update("Client Key", "utf8")
    .digest();
  const storedKey = createHash("sha256").update(clientKey).digest();
  const serverKey = createHmac("sha256", saltedPassword)
    .update("Server Key", "utf8")
    .digest();
  return `SCRAM-SHA-256$${SCRAM_ITERATIONS}:${salt.toString("base64")}`
    + `$${storedKey.toString("base64")}:${serverKey.toString("base64")}`;
}

export function neonLeaseRole(userId: string, leaseId: string) {
  const user = userId.replace(/[^A-Za-z0-9]/g, "").toLowerCase().slice(0, 8);
  // Keep the full UUID entropy. A short lease prefix can collide and a lazy
  // cleanup derived from that pending lease could otherwise disable another
  // user's still-active database role.
  const lease = leaseId.replace(/[^A-Za-z0-9]/g, "").toLowerCase().slice(0, 32);
  if (!user || !lease) throw new Error("Invalid Neon lease identity");
  return `dopedb_${user}_${lease}`;
}

export function neonRoleStatements(input: {
  role: string;
  passwordVerifier: string;
  expiresAt: string;
  accessMode: ManagedAccessMode;
  database: string;
  schemas: string[];
}) {
  if (
    !/^dopedb_[a-z0-9]{1,8}_[a-z0-9]{1,32}$/.test(input.role)
    || !/^SCRAM-SHA-256\$4096:[A-Za-z0-9+/]{22}==\$[A-Za-z0-9+/]{43}=:[A-Za-z0-9+/]{43}=$/
      .test(input.passwordVerifier)
  ) {
    throw new Error("Invalid Neon role material");
  }
  if (
    !neonDatabaseName(input.database)
    || input.schemas.length === 0
    || input.schemas.length > 32
    || input.schemas.some((schema) => !neonSchemaName(schema))
    || new Set(input.schemas).size !== input.schemas.length
  ) {
    throw new Error("Invalid Neon privilege scope");
  }
  const expiry = new Date(input.expiresAt);
  if (
    Number.isNaN(expiry.valueOf())
    || expiry.valueOf() <= Date.now()
    || expiry.valueOf() > Date.now() + 20 * 60 * 1_000
  ) {
    throw new Error("Invalid Neon role expiry");
  }
  const validUntil = expiry.toISOString();
  const identifier = (value: string) => `"${value.replaceAll("\"", "\"\"")}"`;
  const database = identifier(input.database);
  const scopedGrants = input.schemas.flatMap((schemaName) => {
    const schema = identifier(schemaName);
    return input.accessMode === "write"
      ? [
        `GRANT USAGE ON SCHEMA ${schema} TO ${input.role}`,
        `GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA ${schema} `
          + `TO ${input.role}`,
        `GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA ${schema} `
          + `TO ${input.role}`,
      ]
      : [
        `GRANT USAGE ON SCHEMA ${schema} TO ${input.role}`,
        `GRANT SELECT ON ALL TABLES IN SCHEMA ${schema} TO ${input.role}`,
        `GRANT SELECT ON ALL SEQUENCES IN SCHEMA ${schema} TO ${input.role}`,
      ];
  });
  return [
    `CREATE ROLE ${input.role} LOGIN PASSWORD '${input.passwordVerifier}' `
      + `VALID UNTIL '${validUntil}' CONNECTION LIMIT ${NEON_ROLE_CONNECTION_LIMIT}`,
    `GRANT CONNECT ON DATABASE ${database} TO ${input.role}`,
    ...scopedGrants,
    `ALTER ROLE ${input.role} SET statement_timeout = '5min'`,
    `ALTER ROLE ${input.role} SET idle_in_transaction_session_timeout = '1min'`,
    `ALTER ROLE ${input.role} SET idle_session_timeout = '5min'`,
    ...(input.accessMode === "read"
      ? [`ALTER ROLE ${input.role} SET default_transaction_read_only = on`]
      : []),
  ];
}

export function parseNeonConnectionUri(
  value: unknown,
  expectedDatabase: string,
  expectedRole: string,
) {
  if (typeof value !== "string" || value.length > 8_192) {
    throw new Error("Invalid Neon connection URI");
  }
  const url = new URL(value);
  const database = decodeURIComponent(url.pathname.replace(/^\/+/, ""));
  const username = decodeURIComponent(url.username);
  const port = url.port ? Number(url.port) : 5432;
  if (
    !["postgres:", "postgresql:"].includes(url.protocol)
    || !url.hostname.endsWith(".neon.tech")
    || port !== 5432
    || database !== expectedDatabase
    || username !== expectedRole
    || !url.password
    || !["require", "verify-full"].includes(url.searchParams.get("sslmode") ?? "")
  ) {
    throw new Error("Invalid Neon connection URI");
  }
  return {
    connectionUri: value,
    host: url.hostname,
    port,
  };
}
