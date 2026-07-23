// PlanetScale's OAuth and database credential API adapter. Provider responses are
// narrowed immediately so tokens and one-time passwords never enter logs or storage.
import "server-only";

import { env } from "../env";
import { planetScaleEngine } from "./planetscale-core";

const AUTH_ORIGIN = "https://auth.planetscale.com";
const API_ORIGIN = "https://api.planetscale.com";
const REQUEST_TIMEOUT_MS = 15_000;
export const PLANETSCALE_LEASE_SECONDS = 15 * 60;

type JsonObject = Record<string, unknown>;

export type PlanetScaleToken = {
  accessToken: string;
  refreshToken: string;
  expiresAt: string;
  scope: string;
};

export type PlanetScaleTokenInfo = {
  subject: string;
  scope: string;
  expiresAt: string;
};

export type PlanetScaleResource = {
  organization: string;
  database: string;
  branch: string;
  engine: "postgres" | "mysql";
};

export type PlanetScaleLease = {
  externalCredentialId: string;
  externalCredentialKind: "role" | "password";
  host: string;
  port: number;
  database: string;
  username: string;
  password: string;
  sslmode: "verify-full";
  expiresAt: string;
};

export type PlanetScaleResourceItem = {
  id: string;
  name: string;
  kind?: "postgres" | "mysql";
  production?: boolean;
  ready?: boolean;
};

export class PlanetScaleRequestError extends Error {
  constructor(
    message: string,
    public readonly status: number,
  ) {
    super(message);
    this.name = "PlanetScaleRequestError";
  }
}

function credentials() {
  const clientId = env.planetScaleClientId();
  const clientSecret = env.planetScaleClientSecret();
  if (!clientId || !clientSecret) {
    throw new PlanetScaleRequestError("PlanetScale integration is not configured", 503);
  }
  return { clientId, clientSecret };
}

export function isPlanetScaleConfigured() {
  return Boolean(env.planetScaleClientId() && env.planetScaleClientSecret());
}

export function planetScaleRedirectUri() {
  return `${env.appOrigin()}/api/v1/providers/planet-scale/callback`;
}

export function planetScaleAuthorizationUrl(state: string) {
  const { clientId } = credentials();
  const url = new URL("/oauth/authorize", AUTH_ORIGIN);
  url.searchParams.set("client_id", clientId);
  url.searchParams.set("redirect_uri", planetScaleRedirectUri());
  url.searchParams.set("state", state);
  return url.toString();
}

function object(value: unknown): JsonObject {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new PlanetScaleRequestError("PlanetScale returned an invalid response", 502);
  }
  return value as JsonObject;
}

function requiredString(value: unknown, field: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new PlanetScaleRequestError(`PlanetScale response omitted ${field}`, 502);
  }
  return value;
}

function parseExpiresAt(value: unknown, fallbackSeconds: number): string {
  const fallback = new Date(Date.now() + fallbackSeconds * 1_000);
  if (typeof value !== "string") return fallback.toISOString();
  const parsed = new Date(value);
  return Number.isNaN(parsed.valueOf()) ? fallback.toISOString() : parsed.toISOString();
}

async function responseJson(response: Response): Promise<unknown> {
  const body = await response.json().catch(() => null);
  if (!response.ok) {
    // Provider bodies can contain request details. Keep only the status class.
    const status = response.status >= 500 ? 502 : response.status;
    throw new PlanetScaleRequestError("PlanetScale rejected the request", status);
  }
  return body;
}

async function oauthTokenRequest(
  parameters: URLSearchParams,
  previousRefreshToken?: string,
  previousScope = "",
): Promise<PlanetScaleToken> {
  const response = await fetch(`${AUTH_ORIGIN}/oauth/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: parameters,
    cache: "no-store",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  }).catch((error) => {
    if (error instanceof PlanetScaleRequestError) throw error;
    throw new PlanetScaleRequestError("PlanetScale authorization is unavailable", 502);
  });
  const body = object(await responseJson(response));
  const expiresIn = typeof body.expires_in === "number" && body.expires_in > 0
    ? Math.min(body.expires_in, 60 * 60 * 24 * 31)
    : 60 * 60;
  return {
    accessToken: requiredString(body.access_token, "access_token"),
    refreshToken: typeof body.refresh_token === "string" && body.refresh_token.length > 0
      ? body.refresh_token
      : requiredString(previousRefreshToken, "refresh_token"),
    expiresAt: new Date(Date.now() + expiresIn * 1_000).toISOString(),
    scope: typeof body.scope === "string" ? body.scope : previousScope,
  };
}

export async function exchangePlanetScaleCode(code: string): Promise<PlanetScaleToken> {
  const { clientId, clientSecret } = credentials();
  return oauthTokenRequest(new URLSearchParams({
    grant_type: "authorization_code",
    code,
    redirect_uri: planetScaleRedirectUri(),
    client_id: clientId,
    client_secret: clientSecret,
  }));
}

export async function refreshPlanetScaleToken(
  refreshToken: string,
  previousScope = "",
): Promise<PlanetScaleToken> {
  const { clientId, clientSecret } = credentials();
  return oauthTokenRequest(new URLSearchParams({
    grant_type: "refresh_token",
    refresh_token: refreshToken,
    client_id: clientId,
    client_secret: clientSecret,
  }), refreshToken, previousScope);
}

export async function inspectPlanetScaleToken(
  accessToken: string,
): Promise<PlanetScaleTokenInfo> {
  const response = await fetch(`${AUTH_ORIGIN}/oauth/token/info`, {
    headers: { authorization: `Bearer ${accessToken}` },
    cache: "no-store",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  }).catch(() => {
    throw new PlanetScaleRequestError("PlanetScale authorization is unavailable", 502);
  });
  const body = object(await responseJson(response));
  if (body.active !== true) {
    throw new PlanetScaleRequestError("PlanetScale authorization is inactive", 401);
  }
  const exp = typeof body.exp === "number" ? body.exp * 1_000 : Date.now() + 60 * 60 * 1_000;
  return {
    subject: requiredString(body.sub, "subject"),
    scope: typeof body.scope === "string" ? body.scope : "",
    expiresAt: new Date(exp).toISOString(),
  };
}

async function apiRequest(
  accessToken: string,
  path: string,
  init: RequestInit = {},
): Promise<unknown> {
  const response = await fetch(`${API_ORIGIN}${path}`, {
    ...init,
    headers: {
      accept: "application/json",
      authorization: `Bearer ${accessToken}`,
      ...(init.body ? { "content-type": "application/json" } : {}),
      ...init.headers,
    },
    cache: "no-store",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  }).catch(() => {
    throw new PlanetScaleRequestError("PlanetScale API is unavailable", 502);
  });
  if (response.status === 204) return null;
  return responseJson(response);
}

function segment(value: string) {
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(value)) {
    throw new PlanetScaleRequestError("Invalid PlanetScale resource identifier", 400);
  }
  return encodeURIComponent(value);
}

async function paginated(
  accessToken: string,
  path: string,
): Promise<JsonObject[]> {
  const rows: JsonObject[] = [];
  for (let page = 1; page <= 10; page += 1) {
    const separator = path.includes("?") ? "&" : "?";
    const body = object(await apiRequest(
      accessToken,
      `${path}${separator}per_page=100&page=${page}`,
    ));
    const data = Array.isArray(body.data) ? body.data : [];
    rows.push(...data.map(object));
    if (typeof body.next_page !== "number") break;
  }
  return rows;
}

function resourceItem(
  row: JsonObject,
  options: { includeKind?: boolean; includeBranch?: boolean } = {},
): PlanetScaleResourceItem {
  const name = requiredString(row.name ?? row.slug, "resource name");
  const kind = options.includeKind ? planetScaleEngine(row.kind) ?? undefined : undefined;
  return {
    id: typeof row.id === "string" ? row.id : name,
    name,
    ...(kind ? { kind } : {}),
    ...(options.includeBranch ? {
      production: row.production === true,
      ready: row.ready !== false,
    } : {}),
  };
}

export async function listPlanetScaleOrganizations(accessToken: string) {
  const rows = await paginated(accessToken, "/v1/organizations");
  return rows.map((row) => resourceItem(row));
}

export async function listPlanetScaleDatabases(
  accessToken: string,
  organization: string,
) {
  const rows = await paginated(
    accessToken,
    `/v1/organizations/${segment(organization)}/databases`,
  );
  return rows.map((row) => resourceItem(row, { includeKind: true }));
}

export async function listPlanetScaleBranches(
  accessToken: string,
  organization: string,
  database: string,
) {
  const rows = await paginated(
    accessToken,
    `/v1/organizations/${segment(organization)}/databases/${segment(database)}/branches`,
  );
  return rows.map((row) => resourceItem(row, { includeBranch: true }));
}

export async function validatePlanetScaleResource(
  accessToken: string,
  resource: PlanetScaleResource,
) {
  const databases = await listPlanetScaleDatabases(accessToken, resource.organization);
  const database = databases.find((item) => item.name === resource.database);
  if (!database || database.kind !== resource.engine) {
    throw new PlanetScaleRequestError("PlanetScale database was not found", 404);
  }
  const branches = await listPlanetScaleBranches(
    accessToken,
    resource.organization,
    resource.database,
  );
  if (!branches.some((item) => item.name === resource.branch && item.ready !== false)) {
    throw new PlanetScaleRequestError("PlanetScale branch was not found or is not ready", 404);
  }
}

function connectionParts(value: string, protocol: "postgresql" | "mysql") {
  const url = new URL(value.includes("://") ? value : `${protocol}://${value}`);
  if (
    ![`${protocol}:`, ...(protocol === "postgresql" ? ["postgres:"] : [])]
      .includes(url.protocol)
  ) {
    throw new PlanetScaleRequestError("PlanetScale returned an invalid database address", 502);
  }
  const port = url.port ? Number(url.port) : protocol === "postgresql" ? 5432 : 3306;
  const database = decodeURIComponent(url.pathname.replace(/^\/+/, ""));
  if (
    !url.hostname
    || port < 1
    || port > 65_535
    || (protocol === "postgresql" && !database)
  ) {
    throw new PlanetScaleRequestError("PlanetScale returned an invalid database address", 502);
  }
  return { host: url.hostname, port, database };
}

export async function issuePlanetScaleLease(
  accessToken: string,
  resource: PlanetScaleResource,
  accessMode: "read" | "write",
  label: string,
): Promise<PlanetScaleLease> {
  const base = `/v1/organizations/${segment(resource.organization)}/databases/${
    segment(resource.database)
  }/branches/${segment(resource.branch)}`;
  if (resource.engine === "postgres") {
    const body = object(await apiRequest(accessToken, `${base}/roles`, {
      method: "POST",
      body: JSON.stringify({
        name: label,
        ttl: PLANETSCALE_LEASE_SECONDS,
        inherited_roles: accessMode === "write"
          ? ["pg_read_all_data", "pg_write_all_data"]
          : ["pg_read_all_data"],
        require_where_on_delete: "on",
        require_where_on_update: "on",
      }),
    }));
    const address = connectionParts(
      requiredString(body.access_host_url, "access_host_url"),
      "postgresql",
    );
    return {
      externalCredentialId: requiredString(body.id, "role id"),
      externalCredentialKind: "role",
      ...address,
      database: typeof body.database_name === "string" ? body.database_name : address.database,
      username: requiredString(body.username, "username"),
      password: requiredString(body.password, "password"),
      sslmode: "verify-full",
      expiresAt: parseExpiresAt(body.expires_at, PLANETSCALE_LEASE_SECONDS),
    };
  }

  const body = object(await apiRequest(accessToken, `${base}/passwords`, {
    method: "POST",
    body: JSON.stringify({
      name: label,
      role: accessMode === "write" ? "readwriter" : "reader",
      ttl: PLANETSCALE_LEASE_SECONDS,
    }),
  }));
  const address = connectionParts(
    requiredString(body.access_host_url, "access_host_url"),
    "mysql",
  );
  return {
    externalCredentialId: requiredString(body.id, "password id"),
    externalCredentialKind: "password",
    ...address,
    database: resource.database,
    username: requiredString(body.username, "username"),
    password: requiredString(body.plain_text, "plain_text"),
    sslmode: "verify-full",
    expiresAt: parseExpiresAt(body.expires_at, PLANETSCALE_LEASE_SECONDS),
  };
}

export async function revokePlanetScaleLease(
  accessToken: string,
  resource: PlanetScaleResource,
  credentialKind: "role" | "password",
  credentialId: string,
) {
  const collection = credentialKind === "role" ? "roles" : "passwords";
  await apiRequest(
    accessToken,
    `/v1/organizations/${segment(resource.organization)}/databases/${
      segment(resource.database)
    }/branches/${segment(resource.branch)}/${collection}/${segment(credentialId)}`,
    { method: "DELETE" },
  );
}

export async function revokePlanetScaleAuthorization(token: string) {
  const { clientId, clientSecret } = credentials();
  const response = await fetch(`${AUTH_ORIGIN}/oauth/revoke`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      token,
      client_id: clientId,
      client_secret: clientSecret,
    }),
    cache: "no-store",
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  }).catch(() => {
    throw new PlanetScaleRequestError("PlanetScale authorization could not be revoked", 502);
  });
  if (!response.ok) {
    throw new PlanetScaleRequestError("PlanetScale authorization could not be revoked", 502);
  }
}
