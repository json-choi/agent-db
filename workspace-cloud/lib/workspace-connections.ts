// Strict parsing and public serialization for shared connection templates. Secret-
// bearing fields are rejected rather than silently discarded to surface client bugs.
import type { WorkspaceRoleName } from "./workspace-authorization";

// SQLite paths identify files on one machine and are not meaningful team endpoints.
const engines = ["postgres", "mysql", "mongodb"] as const;
const providers = ["auto", "generic", "neon", "planetScale"] as const;
const allowedKeys = new Set([
  "name", "engine", "provider", "driverId", "host", "port", "database",
  "sslmode", "readonlyDefault", "allowWrites", "env", "schemaGroup",
]);
const forbiddenKeys = new Set([
  "password", "secret", "secretRef", "token", "connectionString", "connectionUrl",
  "url", "username", "extraParams", "certificate", "privateKey",
]);

type SharedConnectionInput = {
  name: string;
  engine: (typeof engines)[number];
  provider: (typeof providers)[number];
  driverId: string | null;
  host: string;
  port: number;
  database: string;
  sslmode: string;
  readonlyDefault: boolean;
  allowWrites: boolean;
  env: string | null;
  schemaGroup: string | null;
};

function text(value: unknown, max: number, required = false): string | null {
  if (value == null && !required) return null;
  if (typeof value !== "string") throw new Error("Expected text");
  const normalized = value.trim();
  if (
    (required && !normalized) ||
    normalized.length > max ||
    /[\u0000-\u001f\u007f]/.test(normalized)
  ) {
    throw new Error("Invalid text value");
  }
  return normalized || null;
}

export function parseSharedConnection(value: unknown): SharedConnectionInput {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Connection template must be an object");
  }
  const body = value as Record<string, unknown>;
  for (const key of Object.keys(body)) {
    if (forbiddenKeys.has(key)) throw new Error(`Secret-bearing field '${key}' is not accepted`);
    if (!allowedKeys.has(key)) throw new Error(`Unknown connection field '${key}'`);
  }
  if (!engines.includes(body.engine as (typeof engines)[number])) throw new Error("Invalid engine");
  if (!providers.includes(body.provider as (typeof providers)[number])) throw new Error("Invalid provider");
  if (!Number.isInteger(body.port) || Number(body.port) < 1 || Number(body.port) > 65535) {
    throw new Error("Invalid port");
  }
  if (typeof body.readonlyDefault !== "boolean" || typeof body.allowWrites !== "boolean") {
    throw new Error("Invalid safety policy");
  }
  const host = text(body.host, 512, true)!;
  if (/[@/?#\s]/.test(host) || host.includes("://")) {
    throw new Error("Host must not contain credentials or a connection URL");
  }
  const database = text(body.database, 512) ?? "";
  if (/[?#\r\n]/.test(database)) throw new Error("Invalid database name");
  return {
    name: text(body.name, 120, true)!,
    engine: body.engine as SharedConnectionInput["engine"],
    provider: body.provider as SharedConnectionInput["provider"],
    driverId: text(body.driverId, 160),
    host,
    port: Number(body.port),
    database,
    sslmode: text(body.sslmode, 64, true)!,
    readonlyDefault: body.readonlyDefault,
    allowWrites: body.allowWrites,
    env: text(body.env, 32),
    schemaGroup: text(body.schemaGroup, 120),
  };
}

export function publicConnection(
  row: {
    id: string; name: string; engine: string; provider: string; driverId: string | null;
    host: string; port: number; databaseName: string; sslmode: string;
    readonlyDefault: boolean; allowWrites: boolean; environment: string | null;
    schemaGroup: string | null; revision: number; updatedAt: Date;
  },
  role: WorkspaceRoleName,
  accessMode: "view" | "read" | "write" | "manage",
) {
  return {
    id: row.id,
    name: row.name,
    engine: row.engine,
    provider: row.provider,
    driverId: row.driverId,
    host: row.host,
    port: row.port,
    database: row.databaseName,
    sslmode: row.sslmode,
    readonlyDefault: row.readonlyDefault,
    allowWrites: row.allowWrites && (accessMode === "write" || accessMode === "manage"),
    env: row.environment,
    schemaGroup: row.schemaGroup,
    revision: row.revision,
    updatedAt: row.updatedAt.toISOString(),
    role,
    accessMode,
    credentialsRequired: true,
  };
}
