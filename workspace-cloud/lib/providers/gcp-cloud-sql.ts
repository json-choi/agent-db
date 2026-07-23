// GCP Cloud SQL adapter using Vercel OIDC and Workload Identity Federation.
// Customer service-account keys are never created, uploaded, or persisted.
import "server-only";

import {
  GCP_LEASE_SECONDS,
  gcpCloudSqlEngine,
  gcpConnectionTarget,
  gcpDatabaseUsername,
  gcpWifAudience,
  normalizeGcpUpstreamStatus,
  parseGcpCloudSqlCredential,
  type GcpCloudSqlCredential,
  type GcpCloudSqlResource,
} from "./gcp-cloud-sql-core";
import {
  ProviderRequestError,
  type ManagedAccessMode,
  type ManagedProviderLease,
  type ProviderResourceItem,
} from "./provider-types";

const STS_URL = "https://sts.googleapis.com/v1/token";
const IAM_CREDENTIALS_ORIGIN = "https://iamcredentials.googleapis.com";
const SQL_ADMIN_ORIGIN = "https://sqladmin.googleapis.com/sql/v1beta4";
const CLOUD_PLATFORM_SCOPE = "https://www.googleapis.com/auth/cloud-platform";
const SQL_LOGIN_SCOPE = "https://www.googleapis.com/auth/sqlservice.login";
const REQUEST_TIMEOUT_MS = 15_000;
type JsonObject = Record<string, unknown>;

type GcpAccessToken = {
  accessToken: string;
  expiresAt: string;
};

function requireCurrentSecurityConfiguration(
  credential: GcpCloudSqlCredential,
) {
  try {
    parseGcpCloudSqlCredential(credential);
  } catch {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Reconnect GCP with a dedicated instance and instance-scoped IAM confirmation",
      409,
    );
  }
}

function object(value: unknown): JsonObject {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ProviderRequestError("gcpCloudSql", "GCP returned an invalid response", 502);
  }
  return value as JsonObject;
}

function requiredString(value: unknown, field: string) {
  if (typeof value !== "string" || !value || value.length > 64 * 1_024) {
    throw new ProviderRequestError("gcpCloudSql", `GCP response omitted ${field}`, 502);
  }
  return value;
}

function requestOidcToken(value: string | null) {
  if (
    !value
    || value.length < 100
    || value.length > 32 * 1_024
    || value.split(".").length !== 3
    || /\s/.test(value)
  ) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Vercel OIDC is not available for GCP federation",
      503,
    );
  }
  return value;
}

export function vercelOidcToken(request: Request): string | null {
  if (process.env.VERCEL === "1") {
    return request.headers.get("x-vercel-oidc-token");
  }
  if (process.env.NODE_ENV !== "production") {
    return process.env.VERCEL_OIDC_TOKEN?.trim() || null;
  }
  return null;
}

async function jsonRequest(
  provider: string,
  url: string,
  init: RequestInit,
) {
  const response = await fetch(url, {
    ...init,
    cache: "no-store",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  }).catch(() => {
    throw new ProviderRequestError(provider, "GCP API is unavailable", 502);
  });
  const body = await response.json().catch(() => null);
  if (!response.ok) {
    const status = normalizeGcpUpstreamStatus(response.status);
    throw new ProviderRequestError(provider, "GCP rejected the request", status);
  }
  return object(body);
}

async function federatedToken(
  credential: GcpCloudSqlCredential,
  oidcToken: string,
) {
  const body = await jsonRequest("gcpCloudSql", STS_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      audience: gcpWifAudience(credential),
      grantType: "urn:ietf:params:oauth:grant-type:token-exchange",
      requestedTokenType: "urn:ietf:params:oauth:token-type:access_token",
      scope: CLOUD_PLATFORM_SCOPE,
      subjectToken: requestOidcToken(oidcToken),
      subjectTokenType: "urn:ietf:params:oauth:token-type:jwt",
    }),
  });
  return requiredString(body.access_token, "federated access token");
}

async function serviceAccountToken(input: {
  credential: GcpCloudSqlCredential;
  oidcToken: string;
  serviceAccountEmail: string;
  scope: string;
}): Promise<GcpAccessToken> {
  requireCurrentSecurityConfiguration(input.credential);
  const exchanged = await federatedToken(input.credential, input.oidcToken);
  const body = await jsonRequest(
    "gcpCloudSql",
    `${IAM_CREDENTIALS_ORIGIN}/v1/projects/-/serviceAccounts/${
      encodeURIComponent(input.serviceAccountEmail)
    }:generateAccessToken`,
    {
      method: "POST",
      headers: {
        authorization: `Bearer ${exchanged}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        scope: [input.scope],
        lifetime: `${GCP_LEASE_SECONDS}s`,
      }),
    },
  );
  const accessToken = requiredString(body.accessToken, "service account access token");
  const expiresAt = requiredString(body.expireTime, "service account token expiry");
  const validMs = new Date(expiresAt).valueOf() - Date.now();
  if (
    !Number.isFinite(validMs)
    || validMs < 60_000
    || validMs > (GCP_LEASE_SECONDS + 60) * 1_000
  ) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "GCP returned an unsafe token expiry",
      502,
    );
  }
  return { accessToken, expiresAt: new Date(expiresAt).toISOString() };
}

async function controlPlaneToken(
  credential: GcpCloudSqlCredential,
  oidcToken: string,
) {
  return serviceAccountToken({
    credential,
    oidcToken,
    serviceAccountEmail: credential.readServiceAccountEmail,
    scope: CLOUD_PLATFORM_SCOPE,
  });
}

async function sqlAdminRequest(
  accessToken: string,
  path: string,
): Promise<JsonObject> {
  return jsonRequest(
    "gcpCloudSql",
    `${SQL_ADMIN_ORIGIN}${path}`,
    { headers: { authorization: `Bearer ${accessToken}` } },
  );
}

function pathSegment(value: string) {
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(value)) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Invalid Cloud SQL resource identifier",
      400,
    );
  }
  return encodeURIComponent(value);
}

export async function validateGcpCloudSqlCredential(
  credential: GcpCloudSqlCredential,
  oidcToken: string,
) {
  const token = await controlPlaneToken(credential, oidcToken);
  const [details] = await Promise.all([
    instanceDetailsWithToken(
      credential,
      token.accessToken,
      credential.instanceId,
    ),
    serviceAccountToken({
      credential,
      oidcToken,
      serviceAccountEmail: credential.readServiceAccountEmail,
      scope: SQL_LOGIN_SCOPE,
    }),
    ...(credential.writeServiceAccountEmail ? [
      serviceAccountToken({
        credential,
        oidcToken,
        serviceAccountEmail: credential.writeServiceAccountEmail,
        scope: SQL_LOGIN_SCOPE,
      }),
    ] : []),
  ]);
  if (
    details.name !== credential.instanceId
    || !gcpCloudSqlEngine(details.databaseVersion)
    || details.state !== "RUNNABLE"
  ) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "The dedicated Cloud SQL instance was not found or is not runnable",
      409,
    );
  }
}

export function listGcpProjects(
  credential: GcpCloudSqlCredential,
): ProviderResourceItem[] {
  return [{
    id: credential.projectId,
    value: credential.projectId,
    name: credential.projectId,
    ready: true,
  }];
}

export async function listGcpCloudSqlInstances(
  credential: GcpCloudSqlCredential,
  oidcToken: string,
): Promise<ProviderResourceItem[]> {
  const token = await controlPlaneToken(credential, oidcToken);
  return listGcpCloudSqlInstancesWithToken(credential, token.accessToken);
}

async function listGcpCloudSqlInstancesWithToken(
  credential: GcpCloudSqlCredential,
  accessToken: string,
): Promise<ProviderResourceItem[]> {
  const row = await instanceDetailsWithToken(
    credential,
    accessToken,
    credential.instanceId,
  );
  const kind = gcpCloudSqlEngine(row.databaseVersion);
  if (!kind) return [];
  const name = requiredString(row.name, "instance name");
  if (name !== credential.instanceId) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "GCP returned an unexpected Cloud SQL instance",
      502,
    );
  }
  const settings = row.settings;
  const labels = settings && typeof settings === "object" && !Array.isArray(settings)
    ? (settings as JsonObject).userLabels
    : null;
  const environment = labels && typeof labels === "object" && !Array.isArray(labels)
    ? String((labels as JsonObject).environment ?? "")
    : "";
  return [{
    id: name,
    value: name,
    name,
    kind,
    production: /^(prod|production)$/i.test(environment),
    ready: row.state === "RUNNABLE",
  }];
}

export async function listGcpCloudSqlDatabases(
  credential: GcpCloudSqlCredential,
  oidcToken: string,
  instance: string,
  engine: "postgres" | "mysql" | null,
): Promise<ProviderResourceItem[]> {
  const token = await controlPlaneToken(credential, oidcToken);
  return listGcpCloudSqlDatabasesWithToken(
    credential,
    token.accessToken,
    instance,
    engine,
  );
}

async function listGcpCloudSqlDatabasesWithToken(
  credential: GcpCloudSqlCredential,
  accessToken: string,
  instance: string,
  engine: "postgres" | "mysql" | null,
): Promise<ProviderResourceItem[]> {
  requireDedicatedInstance(credential, instance);
  const body = await sqlAdminRequest(
    accessToken,
    `/projects/${pathSegment(credential.projectId)}/instances/${
      pathSegment(instance)
    }/databases`,
  );
  const rows = Array.isArray(body.items) ? body.items.map(object) : [];
  return rows.map((row) => {
    const name = requiredString(row.name, "database name");
    return {
      id: name,
      value: name,
      name,
      ...(engine ? { kind: engine } : {}),
      ready: true,
    };
  });
}

function requireDedicatedInstance(
  credential: GcpCloudSqlCredential,
  instance: string,
) {
  if (instance !== credential.instanceId) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "This integration is restricted to its dedicated Cloud SQL instance",
      403,
    );
  }
}

async function connectSettingsWithToken(
  credential: GcpCloudSqlCredential,
  accessToken: string,
  instance: string,
) {
  return sqlAdminRequest(
    accessToken,
    `/projects/${pathSegment(credential.projectId)}/instances/${
      pathSegment(instance)
    }/connectSettings`,
  );
}

async function instanceDetailsWithToken(
  credential: GcpCloudSqlCredential,
  accessToken: string,
  instance: string,
) {
  return sqlAdminRequest(
    accessToken,
    `/projects/${pathSegment(credential.projectId)}/instances/${
      pathSegment(instance)
    }`,
  );
}

async function iamDatabaseUsersWithToken(
  credential: GcpCloudSqlCredential,
  accessToken: string,
  instance: string,
) {
  const body = await sqlAdminRequest(
    accessToken,
    `/projects/${pathSegment(credential.projectId)}/instances/${
      pathSegment(instance)
    }/users`,
  );
  return Array.isArray(body.items) ? body.items.map(object) : [];
}

function requireIamDatabaseConfiguration(
  instance: JsonObject,
  users: JsonObject[],
  engine: "postgres" | "mysql",
  serviceAccountEmails: string[],
) {
  const settings = instance.settings;
  const flags = settings && typeof settings === "object" && !Array.isArray(settings)
    && Array.isArray((settings as JsonObject).databaseFlags)
    ? ((settings as JsonObject).databaseFlags as unknown[]).map(object)
    : [];
  const requiredFlag = engine === "postgres"
    ? "cloudsql.iam_authentication"
    : "cloudsql_iam_authentication";
  const enabled = flags.some((flag) => (
    flag.name === requiredFlag
    && ["on", "true", "1"].includes(String(flag.value).toLowerCase())
  ));
  if (!enabled) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      `Cloud SQL IAM database authentication flag '${requiredFlag}' is not enabled`,
      409,
    );
  }
  const configuredUsers = new Set(users.flatMap((user) => (
    user.type === "CLOUD_IAM_SERVICE_ACCOUNT" && typeof user.name === "string"
      ? [user.name.toLowerCase()]
      : []
  )));
  if (!serviceAccountEmails.every((email) => (
    configuredUsers.has(email.toLowerCase())
    || configuredUsers.has(gcpDatabaseUsername(email, engine).toLowerCase())
  ))) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Cloud SQL IAM database users are not configured for the service accounts",
      409,
    );
  }
}

export async function validateGcpCloudSqlResource(
  credential: GcpCloudSqlCredential,
  oidcToken: string,
  resource: GcpCloudSqlResource,
) {
  if (resource.project !== credential.projectId) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Cloud SQL project does not match the integration",
      403,
    );
  }
  requireDedicatedInstance(credential, resource.instance);
  const control = await controlPlaneToken(credential, oidcToken);
  const [databases, settings, details, users] = await Promise.all([
    listGcpCloudSqlDatabasesWithToken(
      credential,
      control.accessToken,
      resource.instance,
      resource.engine,
    ),
    connectSettingsWithToken(credential, control.accessToken, resource.instance),
    instanceDetailsWithToken(credential, control.accessToken, resource.instance),
    iamDatabaseUsersWithToken(credential, control.accessToken, resource.instance),
  ]);
  if (
    gcpCloudSqlEngine(details.databaseVersion) !== resource.engine
    || details.state !== "RUNNABLE"
  ) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Cloud SQL instance was not found or is not runnable",
      404,
    );
  }
  if (!databases.some((item) => item.value === resource.database)) {
    throw new ProviderRequestError("gcpCloudSql", "Cloud SQL database was not found", 404);
  }
  requireIamDatabaseConfiguration(
    details,
    users,
    resource.engine,
    [
      credential.readServiceAccountEmail,
      credential.writeServiceAccountEmail,
    ].filter((value): value is string => Boolean(value)),
  );
  try {
    gcpConnectionTarget({
      connectSettings: settings,
      instanceDetails: details,
      networkMode: resource.networkMode,
    });
  } catch (error) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      error instanceof Error ? error.message : "Cloud SQL connection is unavailable",
      409,
    );
  }
}

export async function issueGcpCloudSqlLease(input: {
  credential: GcpCloudSqlCredential;
  oidcToken: string;
  resource: GcpCloudSqlResource;
  accessMode: ManagedAccessMode;
  externalCredentialId: string;
}): Promise<ManagedProviderLease> {
  if (input.resource.project !== input.credential.projectId) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Cloud SQL project does not match the integration",
      403,
    );
  }
  requireDedicatedInstance(input.credential, input.resource.instance);
  const serviceAccountEmail = input.accessMode === "write"
    ? input.credential.writeServiceAccountEmail
    : input.credential.readServiceAccountEmail;
  if (!serviceAccountEmail) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Cloud SQL write service account is not configured",
      409,
    );
  }
  const [loginToken, control] = await Promise.all([
    serviceAccountToken({
      credential: input.credential,
      oidcToken: input.oidcToken,
      serviceAccountEmail,
      scope: SQL_LOGIN_SCOPE,
    }),
    controlPlaneToken(input.credential, input.oidcToken),
  ]);
  const [settings, details, users, databases] = await Promise.all([
    connectSettingsWithToken(
      input.credential,
      control.accessToken,
      input.resource.instance,
    ),
    instanceDetailsWithToken(
      input.credential,
      control.accessToken,
      input.resource.instance,
    ),
    iamDatabaseUsersWithToken(
      input.credential,
      control.accessToken,
      input.resource.instance,
    ),
    listGcpCloudSqlDatabasesWithToken(
      input.credential,
      control.accessToken,
      input.resource.instance,
      input.resource.engine,
    ),
  ]);
  const actualEngine = gcpCloudSqlEngine(settings.databaseVersion);
  if (
    actualEngine !== input.resource.engine
    || gcpCloudSqlEngine(details.databaseVersion) !== input.resource.engine
    || details.state !== "RUNNABLE"
    || !databases.some((item) => item.value === input.resource.database)
  ) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      "Cloud SQL database is no longer available",
      409,
    );
  }
  requireIamDatabaseConfiguration(
    details,
    users,
    input.resource.engine,
    [serviceAccountEmail],
  );
  let target;
  try {
    target = gcpConnectionTarget({
      connectSettings: settings,
      instanceDetails: details,
      networkMode: input.resource.networkMode,
    });
  } catch (error) {
    throw new ProviderRequestError(
      "gcpCloudSql",
      error instanceof Error ? error.message : "Cloud SQL connection is unavailable",
      409,
    );
  }
  return {
    externalCredentialId: input.externalCredentialId,
    externalCredentialKind: "iamToken",
    host: target.host,
    port: input.resource.engine === "postgres" ? 5432 : 3306,
    database: input.resource.database,
    username: gcpDatabaseUsername(serviceAccountEmail, input.resource.engine),
    password: loginToken.accessToken,
    sslmode: target.sslmode,
    tlsServerCaPem: target.tlsServerCaPem,
    expiresAt: loginToken.expiresAt,
  };
}
