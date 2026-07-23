// Neon control-plane adapter. The encrypted API key discovers one project hierarchy
// and obtains an owner session only long enough to create or revoke a constrained role.
import "server-only";

import { randomBytes } from "node:crypto";
import { neon } from "@neondatabase/serverless";
import {
  NEON_LEASE_SECONDS,
  NEON_PUBLIC_DATABASE_ESCAPE_SQL,
  NEON_PUBLIC_SCHEMA_CREATE_SQL,
  NEON_PUBLIC_SCHEMA_ESCAPE_SQL,
  createNeonScramVerifier,
  neonIntegrationIdentity,
  neonLeaseRole,
  neonPublicDatabaseBoundaryError,
  neonRoleStatements,
  neonSegment,
  parseNeonConnectionUri,
  type NeonCredential,
  type NeonResource,
} from "./neon-core";
import {
  ProviderRequestError,
  type ManagedAccessMode,
  type ManagedProviderLease,
  type ProviderResourceItem,
} from "./provider-types";

const API_ORIGIN = "https://console.neon.tech/api/v2";
const REQUEST_TIMEOUT_MS = 15_000;
type JsonObject = Record<string, unknown>;

export type NeonAuthInfo = {
  displayName: string;
  externalAccountId: string;
  projectCount: number;
  scopeFingerprint: string;
};

function object(value: unknown): JsonObject {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ProviderRequestError("neon", "Neon returned an invalid response", 502);
  }
  return value as JsonObject;
}

function requiredString(value: unknown, field: string) {
  if (typeof value !== "string" || !value || value.length > 2_048) {
    throw new ProviderRequestError("neon", `Neon response omitted ${field}`, 502);
  }
  return value;
}

function apiSegment(value: string) {
  if (!neonSegment(value)) {
    throw new ProviderRequestError("neon", "Invalid Neon resource identifier", 400);
  }
  return encodeURIComponent(value);
}

async function apiRequest(
  credential: NeonCredential,
  path: string,
  init: RequestInit = {},
): Promise<unknown> {
  const response = await fetch(`${API_ORIGIN}${path}`, {
    ...init,
    headers: {
      accept: "application/json",
      authorization: `Bearer ${credential.apiKey}`,
      ...(init.body ? { "content-type": "application/json" } : {}),
      ...init.headers,
    },
    cache: "no-store",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  }).catch(() => {
    throw new ProviderRequestError("neon", "Neon API is unavailable", 502);
  });
  const body = response.status === 204
    ? null
    : await response.json().catch(() => null);
  if (!response.ok) {
    // A revoked provider key is a failed integration dependency, not an expired
    // DopeDB session. Never let a provider 401 sign the desktop user out.
    const status = response.status === 401
      ? 424
      : response.status >= 500
        ? 502
        : response.status;
    throw new ProviderRequestError("neon", "Neon rejected the request", status);
  }
  return body;
}

function nextCursor(body: JsonObject) {
  const pagination = body.pagination;
  if (!pagination || typeof pagination !== "object" || Array.isArray(pagination)) {
    return null;
  }
  const page = pagination as JsonObject;
  const value = page.next ?? page.cursor;
  return typeof value === "string" && value.length <= 2_048 ? value : null;
}

export async function listNeonProjects(
  credential: NeonCredential,
): Promise<ProviderResourceItem[]> {
  const rows: JsonObject[] = [];
  let cursor: string | null = null;
  for (let page = 0; page < 10; page += 1) {
    const query = new URLSearchParams({ limit: "400", timeout: "15000" });
    if (cursor) query.set("cursor", cursor);
    if (credential.organizationId) query.set("org_id", credential.organizationId);
    const body = object(await apiRequest(credential, `/projects?${query}`));
    const projects = Array.isArray(body.projects) ? body.projects : [];
    rows.push(...projects.map(object));
    cursor = nextCursor(body);
    if (!cursor) break;
  }
  if (cursor) {
    throw new ProviderRequestError(
      "neon",
      "Neon project scope is too large to fingerprint safely",
      409,
    );
  }
  return rows.map((row) => {
    const id = requiredString(row.id, "project id");
    return {
      id,
      value: id,
      name: requiredString(row.name, "project name"),
      kind: "postgres",
      ready: true,
    };
  });
}

export async function inspectNeonCredential(
  credential: NeonCredential,
): Promise<NeonAuthInfo> {
  // Project-scoped organization keys intentionally cannot call account-level
  // endpoints. Project discovery proves the key first; /users/me is then optional,
  // while /users/me/organizations is documented for every API-key type.
  const projects = await listNeonProjects(credential);
  if (projects.length === 0) {
    throw new ProviderRequestError("neon", "Neon API key cannot access a project", 403);
  }
  const [userId, organizationsBody] = await Promise.all([
    apiRequest(credential, "/users/me")
      .then((body) => requiredString(object(body).id, "user id"))
      .catch((error) => {
        if (
          error instanceof ProviderRequestError
          && [403, 404, 424].includes(error.status)
        ) {
          return null;
        }
        throw error;
      }),
    apiRequest(credential, "/users/me/organizations").then(object),
  ]);
  const organizations = Array.isArray(organizationsBody.organizations)
    ? organizationsBody.organizations.map(object)
    : [];
  const organizationIds = organizations.map((organization) => (
    requiredString(organization.id, "organization id")
  ));
  if (
    credential.organizationId
    && !organizationIds.includes(credential.organizationId)
  ) {
    throw new ProviderRequestError(
      "neon",
      "Neon API key does not belong to the selected organization",
      403,
    );
  }
  const organizationId = credential.organizationId
    ?? (organizationIds.length === 1 ? organizationIds[0] : null);
  if (!userId && !organizationId) {
    throw new ProviderRequestError(
      "neon",
      "Neon API key identity could not be resolved",
      409,
    );
  }
  const identity = neonIntegrationIdentity(
    credential.organizationId || !userId
      ? { kind: "organization", id: organizationId! }
      : { kind: "user", id: userId },
    projects.map((project) => project.value),
  );
  return {
    displayName: projects.length === 1
      ? `Neon · ${projects[0].name}`
      : `Neon · 프로젝트 ${projects.length}개`,
    externalAccountId: identity.externalAccountId,
    projectCount: projects.length,
    scopeFingerprint: identity.scopeFingerprint,
  };
}

export async function listNeonBranches(
  credential: NeonCredential,
  project: string,
): Promise<ProviderResourceItem[]> {
  const body = object(await apiRequest(
    credential,
    `/projects/${apiSegment(project)}/branches?limit=10000`,
  ));
  const rows = Array.isArray(body.branches) ? body.branches.map(object) : [];
  return rows.map((row) => {
    const id = requiredString(row.id, "branch id");
    return {
      id,
      value: id,
      name: requiredString(row.name, "branch name"),
      production: row.default === true || row.protected === true,
      ready: row.current_state === "ready",
    };
  });
}

export async function listNeonDatabases(
  credential: NeonCredential,
  project: string,
  branch: string,
): Promise<ProviderResourceItem[]> {
  const body = object(await apiRequest(
    credential,
    `/projects/${apiSegment(project)}/branches/${apiSegment(branch)}/databases`,
  ));
  const rows = Array.isArray(body.databases) ? body.databases.map(object) : [];
  return rows.map((row) => {
    const name = requiredString(row.name, "database name");
    return {
      id: String(row.id ?? name),
      value: name,
      name,
      kind: "postgres",
      ready: true,
    };
  });
}

async function databaseOwner(
  credential: NeonCredential,
  resource: NeonResource,
) {
  const body = object(await apiRequest(
    credential,
    `/projects/${apiSegment(resource.project)}/branches/${
      apiSegment(resource.branch)
    }/databases`,
  ));
  const rows = Array.isArray(body.databases) ? body.databases.map(object) : [];
  const database = rows.find((row) => row.name === resource.database);
  return database ? requiredString(database.owner_name, "database owner") : null;
}

async function readWriteEndpoint(
  credential: NeonCredential,
  resource: NeonResource,
) {
  const body = object(await apiRequest(
    credential,
    `/projects/${apiSegment(resource.project)}/branches/${
      apiSegment(resource.branch)
    }/endpoints`,
  ));
  const rows = Array.isArray(body.endpoints) ? body.endpoints.map(object) : [];
  const endpoint = rows.find((row) => row.type === "read_write" && row.disabled !== true);
  if (!endpoint) {
    throw new ProviderRequestError(
      "neon",
      "Neon branch has no available read-write endpoint",
      409,
    );
  }
  const host = requiredString(endpoint.host, "endpoint host");
  if (!host.endsWith(".neon.tech")) {
    throw new ProviderRequestError("neon", "Neon returned an invalid endpoint", 502);
  }
  return {
    id: requiredString(endpoint.id, "endpoint id"),
    host,
  };
}

async function ownerConnection(
  credential: NeonCredential,
  resource: NeonResource,
) {
  const [owner, endpoint] = await Promise.all([
    databaseOwner(credential, resource),
    readWriteEndpoint(credential, resource),
  ]);
  if (!owner) {
    throw new ProviderRequestError("neon", "Neon database was not found", 404);
  }
  const query = new URLSearchParams({
    branch_id: resource.branch,
    endpoint_id: endpoint.id,
    database_name: resource.database,
    role_name: owner,
    pooled: "false",
  });
  const body = object(await apiRequest(
    credential,
    `/projects/${apiSegment(resource.project)}/connection_uri?${query}`,
  ));
  try {
    return {
      owner,
      endpoint,
      ...parseNeonConnectionUri(body.uri, resource.database, owner),
    };
  } catch {
    throw new ProviderRequestError(
      "neon",
      "Neon could not provide the database owner credential",
      409,
    );
  }
}

function sqlClient(connectionUri: string) {
  return neon(connectionUri, {
    fetchOptions: { signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) },
  });
}

type NeonSqlClient = ReturnType<typeof sqlClient>;

class NeonBoundaryError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "NeonBoundaryError";
  }
}

function writeTableGrantCheck(accessMode: ManagedAccessMode) {
  return accessMode === "write"
    ? " OR NOT has_table_privilege(c.oid, 'INSERT WITH GRANT OPTION')"
      + " OR NOT has_table_privilege(c.oid, 'UPDATE WITH GRANT OPTION')"
      + " OR NOT has_table_privilege(c.oid, 'DELETE WITH GRANT OPTION')"
    : "";
}

function writeSequenceGrantCheck(accessMode: ManagedAccessMode) {
  return accessMode === "write"
    ? " OR NOT has_sequence_privilege(c.oid, 'USAGE WITH GRANT OPTION')"
      + " OR NOT has_sequence_privilege(c.oid, 'UPDATE WITH GRANT OPTION')"
    : "";
}

async function assertNeonManagedBoundary(
  sql: NeonSqlClient,
  resource: NeonResource,
  accessMode: ManagedAccessMode,
) {
  const databaseRows = await sql.query(
    "SELECT has_database_privilege("
      + "current_database(), 'CONNECT WITH GRANT OPTION') AS grantable",
  );
  if (databaseRows[0]?.grantable !== true) {
    throw new NeonBoundaryError(
      "Neon database CONNECT privilege cannot be delegated by the database owner",
    );
  }
  const publicDatabasePrivileges = await sql.query(
    NEON_PUBLIC_DATABASE_ESCAPE_SQL,
  );
  const publicDatabaseBoundaryError = neonPublicDatabaseBoundaryError(
    publicDatabasePrivileges.map((row) => row.privilege_type),
  );
  if (publicDatabaseBoundaryError) {
    throw new NeonBoundaryError(publicDatabaseBoundaryError);
  }
  const schemaRows = await sql.query(
    "SELECT n.nspname AS schema_name, "
      + "has_schema_privilege(n.oid, 'USAGE WITH GRANT OPTION') AS grantable "
      + "FROM pg_namespace n WHERE n.nspname = ANY($1::text[])",
    [resource.schemas],
  );
  if (
    schemaRows.length !== resource.schemas.length
    || schemaRows.some((row) => row.grantable !== true)
  ) {
    throw new NeonBoundaryError(
      "Neon schema allowlist is missing or cannot be granted by the database owner",
    );
  }

  const publicSchemaEscape = await sql.query(
    NEON_PUBLIC_SCHEMA_ESCAPE_SQL,
    [resource.schemas],
  );
  const publicSchemaCreate = await sql.query(
    NEON_PUBLIC_SCHEMA_CREATE_SQL,
    [resource.schemas],
  );
  if (publicSchemaEscape.length > 0 || publicSchemaCreate.length > 0) {
    throw new NeonBoundaryError(
      "Neon PUBLIC schema privileges escape the managed schema allowlist",
    );
  }

  const disallowedPublicTablePrivileges = accessMode === "write"
    ? ["TRUNCATE", "REFERENCES", "TRIGGER", "MAINTAIN"]
    : [
      "INSERT",
      "UPDATE",
      "DELETE",
      "TRUNCATE",
      "REFERENCES",
      "TRIGGER",
      "MAINTAIN",
    ];
  const publicTableEscapes = await sql.query(
    "SELECT n.nspname AS schema_name, c.relname AS object_name "
      + "FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace "
      + "CROSS JOIN LATERAL aclexplode("
      + "COALESCE(c.relacl, acldefault('r', c.relowner))) acl "
      + "WHERE n.nspname = ANY($1::text[]) "
      + "AND c.relkind IN ('r', 'p', 'v', 'm', 'f') "
      + "AND acl.grantee = 0 "
      + "AND acl.privilege_type = ANY($2::text[]) LIMIT 1",
    [resource.schemas, disallowedPublicTablePrivileges],
  );
  const publicSequenceEscapes = accessMode === "read"
    ? await sql.query(
      "SELECT n.nspname AS schema_name, c.relname AS object_name "
        + "FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace "
        + "CROSS JOIN LATERAL aclexplode("
        + "COALESCE(c.relacl, acldefault('s', c.relowner))) acl "
        + "WHERE n.nspname = ANY($1::text[]) AND c.relkind = 'S' "
        + "AND acl.grantee = 0 "
        + "AND acl.privilege_type = ANY($2::text[]) LIMIT 1",
      [resource.schemas, ["USAGE", "UPDATE"]],
    )
    : [];
  if (publicTableEscapes.length > 0 || publicSequenceEscapes.length > 0) {
    throw new NeonBoundaryError(
      "Neon PUBLIC object privileges exceed the managed access mode",
    );
  }

  const ungrantableTables = await sql.query(
    "SELECT n.nspname AS schema_name, c.relname AS object_name "
      + "FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace "
      + "WHERE n.nspname = ANY($1::text[]) "
      + "AND c.relkind IN ('r', 'p', 'v', 'm', 'f') "
      + "AND (NOT has_table_privilege(c.oid, 'SELECT WITH GRANT OPTION')"
      + writeTableGrantCheck(accessMode)
      + ") LIMIT 1",
    [resource.schemas],
  );
  const ungrantableSequences = await sql.query(
    "SELECT n.nspname AS schema_name, c.relname AS object_name "
      + "FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace "
      + "WHERE n.nspname = ANY($1::text[]) AND c.relkind = 'S' "
      + "AND (NOT has_sequence_privilege(c.oid, 'SELECT WITH GRANT OPTION')"
      + writeSequenceGrantCheck(accessMode)
      + ") LIMIT 1",
    [resource.schemas],
  );
  if (ungrantableTables.length > 0 || ungrantableSequences.length > 0) {
    throw new NeonBoundaryError(
      "Neon schema contains an object whose privileges cannot be safely delegated",
    );
  }

  const unsafeFunctions = await sql.query(
    "SELECT n.nspname AS schema_name, p.proname AS function_name "
      + "FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace "
      + "CROSS JOIN LATERAL aclexplode("
      + "COALESCE(p.proacl, acldefault('f', p.proowner))) acl "
      + "WHERE n.nspname = ANY($1::text[]) AND p.prosecdef "
      + "AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE' LIMIT 1",
    [resource.schemas],
  );
  if (unsafeFunctions.length > 0) {
    throw new NeonBoundaryError(
      "Neon schema exposes a SECURITY DEFINER function to PUBLIC",
    );
  }

  const reachableDatabases = await sql.query(
    "SELECT d.datname AS database_name FROM pg_database d "
      + "CROSS JOIN LATERAL aclexplode("
      + "COALESCE(d.datacl, acldefault('d', d.datdba))) acl "
      + "WHERE d.datallowconn AND NOT d.datistemplate "
      + "AND d.datname <> current_database() "
      + "AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT' LIMIT 1",
  );
  if (reachableDatabases.length > 0) {
    throw new NeonBoundaryError(
      "Neon managed access requires an isolated branch or PUBLIC CONNECT revoked "
        + "from every other database",
    );
  }
}

function missingWriteRoleChecks(accessMode: ManagedAccessMode) {
  return accessMode === "write"
    ? " OR NOT has_table_privilege($1::name, c.oid, 'INSERT')"
      + " OR NOT has_table_privilege($1::name, c.oid, 'UPDATE')"
      + " OR NOT has_table_privilege($1::name, c.oid, 'DELETE')"
    : "";
}

function missingWriteSequenceChecks(accessMode: ManagedAccessMode) {
  return accessMode === "write"
    ? " OR NOT has_sequence_privilege($1::name, c.oid, 'USAGE')"
      + " OR NOT has_sequence_privilege($1::name, c.oid, 'UPDATE')"
    : "";
}

async function assertNeonRolePrivileges(
  sql: NeonSqlClient,
  role: string,
  resource: NeonResource,
  accessMode: ManagedAccessMode,
) {
  const rows = await sql.query(
    "SELECT "
      + "NOT has_database_privilege($1::name, current_database(), 'CONNECT') "
      + "AS missing_database, "
      + "EXISTS (SELECT 1 FROM pg_namespace n "
      + "WHERE n.nspname = ANY($2::text[]) "
      + "AND NOT has_schema_privilege($1::name, n.oid, 'USAGE')) AS missing_schema, "
      + "EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n "
      + "ON n.oid = c.relnamespace WHERE n.nspname = ANY($2::text[]) "
      + "AND c.relkind IN ('r', 'p', 'v', 'm', 'f') "
      + "AND (NOT has_table_privilege($1::name, c.oid, 'SELECT')"
      + missingWriteRoleChecks(accessMode)
      + ")) AS missing_table, "
      + "EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n "
      + "ON n.oid = c.relnamespace WHERE n.nspname = ANY($2::text[]) "
      + "AND c.relkind = 'S' "
      + "AND (NOT has_sequence_privilege($1::name, c.oid, 'SELECT')"
      + missingWriteSequenceChecks(accessMode)
      + ")) AS missing_sequence",
    [role, resource.schemas],
  );
  const row = rows[0];
  if (
    !row
    || row.missing_database !== false
    || row.missing_schema !== false
    || row.missing_table !== false
    || row.missing_sequence !== false
  ) {
    throw new NeonBoundaryError("Neon role privilege verification failed");
  }
}

function postgresErrorCode(error: unknown): string | null {
  if (!error || typeof error !== "object") return null;
  const body = error as { code?: unknown; cause?: unknown };
  if (typeof body.code === "string") return body.code;
  if (body.cause && typeof body.cause === "object") {
    const cause = body.cause as { code?: unknown };
    if (typeof cause.code === "string") return cause.code;
  }
  return null;
}

async function revokeNeonRoleWithClient(sql: NeonSqlClient, role: string) {
  try {
    // Commit the safety latch independently so later cleanup failures cannot roll
    // LOGIN back on. Missing roles are the idempotent success case.
    await sql.query(`ALTER ROLE ${role} NOLOGIN`);
  } catch (error) {
    if (postgresErrorCode(error) === "42704") return;
    throw error;
  }
  await sql.query(
    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity "
      + "WHERE usename = $1 AND pid <> pg_backend_pid()",
    [role],
  );
  try {
    await sql.transaction((tx) => [
      tx.query(`DROP OWNED BY ${role}`),
      tx.query(`DROP ROLE ${role}`),
    ]);
  } catch (error) {
    if (postgresErrorCode(error) === "42704") return;
    throw error;
  }
}

export async function validateNeonResource(
  credential: NeonCredential,
  resource: NeonResource,
) {
  const projects = await listNeonProjects(credential);
  if (!projects.some((item) => item.value === resource.project)) {
    throw new ProviderRequestError("neon", "Neon project was not found", 404);
  }
  const branches = await listNeonBranches(credential, resource.project);
  if (!branches.some((item) => item.value === resource.branch && item.ready !== false)) {
    throw new ProviderRequestError("neon", "Neon branch was not found or is not ready", 404);
  }
  const databases = await listNeonDatabases(
    credential,
    resource.project,
    resource.branch,
  );
  if (!databases.some((item) => item.value === resource.database)) {
    throw new ProviderRequestError("neon", "Neon database was not found", 404);
  }
  const connection = await ownerConnection(credential, resource);
  try {
    await assertNeonManagedBoundary(sqlClient(connection.connectionUri), resource, "read");
  } catch (error) {
    if (error instanceof NeonBoundaryError) {
      throw new ProviderRequestError("neon", error.message, 409);
    }
    throw new ProviderRequestError(
      "neon",
      "Neon database security boundary could not be verified",
      502,
    );
  }
}

export async function issueNeonLease(input: {
  credential: NeonCredential;
  resource: NeonResource;
  accessMode: ManagedAccessMode;
  role: string;
}): Promise<ManagedProviderLease> {
  const password = randomBytes(32).toString("base64url");
  const passwordVerifier = createNeonScramVerifier(password);
  const expiresAt = new Date(Date.now() + NEON_LEASE_SECONDS * 1_000).toISOString();
  const connection = await ownerConnection(input.credential, input.resource);
  const sql = sqlClient(connection.connectionUri);
  let roleCreated = false;
  try {
    await assertNeonManagedBoundary(sql, input.resource, input.accessMode);
    const statements = neonRoleStatements({
      role: input.role,
      passwordVerifier,
      expiresAt,
      accessMode: input.accessMode,
      database: input.resource.database,
      schemas: input.resource.schemas,
    });
    await sql.transaction(
      statements.map((statement) => sql.query(statement)),
    );
    roleCreated = true;
    await assertNeonRolePrivileges(
      sql,
      input.role,
      input.resource,
      input.accessMode,
    );
  } catch (error) {
    if (roleCreated) {
      await revokeNeonRoleWithClient(sql, input.role).catch(() => undefined);
    }
    if (error instanceof NeonBoundaryError) {
      throw new ProviderRequestError("neon", error.message, 409);
    }
    throw new ProviderRequestError(
      "neon",
      "Neon database role could not be issued",
      502,
    );
  }
  return {
    externalCredentialId: input.role,
    externalCredentialKind: "role",
    host: connection.endpoint.host,
    port: 5432,
    database: input.resource.database,
    username: input.role,
    password,
    sslmode: "verify-full",
    expiresAt,
  };
}

export async function revokeNeonLease(
  credential: NeonCredential,
  resource: NeonResource,
  role: string,
) {
  if (!/^dopedb_[a-z0-9]{1,8}_[a-z0-9]{1,32}$/.test(role)) {
    throw new ProviderRequestError("neon", "Invalid Neon lease role", 400);
  }
  const connection = await ownerConnection(credential, resource);
  try {
    await revokeNeonRoleWithClient(sqlClient(connection.connectionUri), role);
  } catch {
    throw new ProviderRequestError("neon", "Neon database role could not be revoked", 502);
  }
}

export function neonRoleForLease(userId: string, leaseId: string) {
  return neonLeaseRole(userId, leaseId);
}
