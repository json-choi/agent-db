// Pure GCP Cloud SQL trust and resource validation. The integration stores only
// WIF coordinates and service-account identities, never a service-account key.

import { createHash } from "node:crypto";
import type { ManagedEngine, ManagedSslMode } from "./provider-types";

export const GCP_LEASE_SECONDS = 15 * 60;

export type GcpCloudSqlNetworkMode =
  | "PUBLIC"
  | "PRIVATE_SERVICES_ACCESS"
  | "PRIVATE_SERVICE_CONNECT";

export type GcpCloudSqlCredential = {
  projectId: string;
  projectNumber: string;
  workloadIdentityPoolId: string;
  workloadIdentityProviderId: string;
  instanceId: string;
  readServiceAccountEmail: string;
  writeServiceAccountEmail: string | null;
  dedicatedServiceAccountsConfirmed: true;
  instanceScopedIamConfirmed: true;
};

export type GcpCloudSqlResource = {
  project: string;
  instance: string;
  database: string;
  engine: ManagedEngine;
  networkMode: GcpCloudSqlNetworkMode;
};

export function gcpProjectId(value: unknown): value is string {
  return typeof value === "string"
    && /^[a-z][a-z0-9-]{4,28}[a-z0-9]$/.test(value);
}

function gcpWifId(value: unknown): value is string {
  return typeof value === "string"
    && /^[a-z][a-z0-9-]{2,30}[a-z0-9]$/.test(value);
}

function gcpServiceAccountEmail(value: unknown): value is string {
  return typeof value === "string"
    && /^[a-z][a-z0-9-]{4,28}[a-z0-9]@[a-z][a-z0-9-]{4,28}[a-z0-9]\.iam\.gserviceaccount\.com$/
      .test(value);
}

export function parseGcpCloudSqlCredential(
  value: unknown,
): GcpCloudSqlCredential {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("GCP trust configuration is required");
  }
  const body = value as Record<string, unknown>;
  const writeServiceAccountEmail = body.writeServiceAccountEmail === ""
    || body.writeServiceAccountEmail == null
    ? null
    : body.writeServiceAccountEmail;
  if (
    !gcpProjectId(body.projectId)
    || typeof body.projectNumber !== "string"
    || !/^[1-9][0-9]{5,19}$/.test(body.projectNumber)
    || !gcpWifId(body.workloadIdentityPoolId)
    || !gcpWifId(body.workloadIdentityProviderId)
    || !gcpResourceName(body.instanceId)
    || !gcpServiceAccountEmail(body.readServiceAccountEmail)
    || (writeServiceAccountEmail !== null
      && !gcpServiceAccountEmail(writeServiceAccountEmail))
    || writeServiceAccountEmail === body.readServiceAccountEmail
    || body.dedicatedServiceAccountsConfirmed !== true
    || body.instanceScopedIamConfirmed !== true
  ) {
    throw new Error("Invalid GCP trust configuration");
  }
  return {
    projectId: body.projectId,
    projectNumber: body.projectNumber,
    workloadIdentityPoolId: body.workloadIdentityPoolId,
    workloadIdentityProviderId: body.workloadIdentityProviderId,
    instanceId: body.instanceId,
    readServiceAccountEmail: body.readServiceAccountEmail,
    writeServiceAccountEmail,
    dedicatedServiceAccountsConfirmed: true,
    instanceScopedIamConfirmed: true,
  };
}

function gcpResourceName(value: unknown, max = 98): value is string {
  return typeof value === "string"
    && value.length > 0
    && value.length <= max
    && /^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(value);
}

export function parseGcpCloudSqlResource(
  value: unknown,
): GcpCloudSqlResource {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("GCP Cloud SQL resource is required");
  }
  const body = value as Record<string, unknown>;
  const networkMode = body.networkMode;
  if (
    !gcpProjectId(body.project)
    || !gcpResourceName(body.instance)
    || !gcpResourceName(body.database)
    || (body.engine !== "postgres" && body.engine !== "mysql")
    || (
      networkMode !== "PUBLIC"
      && networkMode !== "PRIVATE_SERVICES_ACCESS"
      && networkMode !== "PRIVATE_SERVICE_CONNECT"
    )
  ) {
    throw new Error("Invalid GCP Cloud SQL resource");
  }
  return {
    project: body.project,
    instance: body.instance,
    database: body.database,
    engine: body.engine,
    networkMode,
  };
}

export function gcpCloudSqlEngine(value: unknown): ManagedEngine | null {
  if (typeof value !== "string") return null;
  if (value.startsWith("POSTGRES_")) return "postgres";
  if (value.startsWith("MYSQL_")) return "mysql";
  return null;
}

export function gcpDatabaseUsername(
  serviceAccountEmail: string,
  engine: ManagedEngine,
) {
  if (!gcpServiceAccountEmail(serviceAccountEmail)) {
    throw new Error("Invalid GCP service account");
  }
  return engine === "postgres"
    ? serviceAccountEmail.replace(/\.gserviceaccount\.com$/, "")
    : serviceAccountEmail.slice(0, serviceAccountEmail.indexOf("@"));
}

export function gcpWifAudience(credential: GcpCloudSqlCredential) {
  return `//iam.googleapis.com/projects/${credential.projectNumber}`
    + `/locations/global/workloadIdentityPools/${credential.workloadIdentityPoolId}`
    + `/providers/${credential.workloadIdentityProviderId}`;
}

function identityDigest(value: unknown) {
  return createHash("sha256")
    .update(JSON.stringify(value))
    .digest("hex");
}

export function gcpCloudSqlIntegrationIdentity(
  credential: GcpCloudSqlCredential,
) {
  const readPrincipal = identityDigest(credential.readServiceAccountEmail);
  const writePrincipal = credential.writeServiceAccountEmail
    ? identityDigest(credential.writeServiceAccountEmail)
    : "none";
  const instance = identityDigest({
    projectId: credential.projectId,
    instanceId: credential.instanceId,
  });
  const integration = identityDigest({
    version: 1,
    projectId: credential.projectId,
    projectNumber: credential.projectNumber,
    workloadIdentityPoolId: credential.workloadIdentityPoolId,
    workloadIdentityProviderId: credential.workloadIdentityProviderId,
    instanceId: credential.instanceId,
    readServiceAccountEmail: credential.readServiceAccountEmail,
    writeServiceAccountEmail: credential.writeServiceAccountEmail,
  });
  return {
    externalAccountId:
      `gcp-wif-v1:r${readPrincipal}:w${writePrincipal}:n${instance}:i${integration}`,
    readPrincipal,
    writePrincipal: writePrincipal === "none" ? null : writePrincipal,
    instance,
  };
}

export type GcpCloudSqlPrincipalClaim = {
  principalFingerprint: string;
  targetFingerprint: string;
  accessKind: "read" | "write";
};

export function gcpCloudSqlPrincipalClaims(
  identity: ReturnType<typeof gcpCloudSqlIntegrationIdentity>,
): GcpCloudSqlPrincipalClaim[] {
  return [
    {
      principalFingerprint: identity.readPrincipal,
      targetFingerprint: identity.instance,
      accessKind: "read" as const,
    },
    ...(identity.writePrincipal
      ? [{
        principalFingerprint: identity.writePrincipal,
        targetFingerprint: identity.instance,
        accessKind: "write" as const,
      }]
      : []),
  ];
}

type JsonObject = Record<string, unknown>;

function object(value: unknown): JsonObject | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as JsonObject
    : null;
}

function validDnsName(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const normalized = value.replace(/\.$/, "").toLowerCase();
  if (
    normalized.length === 0
    || normalized.length > 253
    || normalized.includes("*")
    || normalized.split(".").some((label) => (
      label.length === 0
      || label.length > 63
      || !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(label)
    ))
  ) {
    return null;
  }
  return normalized;
}

function validIpv4(value: unknown): value is string {
  return typeof value === "string"
    && value.split(".").length === 4
    && value.split(".").every((part) => (
      /^(?:0|[1-9][0-9]{0,2})$/.test(part) && Number(part) <= 255
    ));
}

type DnsTarget = {
  name: string;
  connectionType:
    | "PUBLIC"
    | "PRIVATE_SERVICES_ACCESS"
    | "PRIVATE_SERVICE_CONNECT";
  recordManager: string;
};

function validConnectionType(
  value: unknown,
): value is DnsTarget["connectionType"] {
  return value === "PUBLIC"
    || value === "PRIVATE_SERVICES_ACCESS"
    || value === "PRIVATE_SERVICE_CONNECT";
}

function dnsTargets(
  connectSettings: JsonObject,
  instanceDetails: JsonObject,
): DnsTarget[] {
  const rows = [connectSettings.dnsNames, instanceDetails.dnsNames]
    .flatMap((value) => Array.isArray(value) ? value : [])
    .flatMap((value) => {
      const row = object(value);
      const name = validDnsName(row?.name);
      const connectionType = row?.connectionType;
      if (
        !row
        || !name
        || row.dnsScope !== "INSTANCE"
        || !validConnectionType(connectionType)
      ) {
        return [];
      }
      return [{
        name,
        connectionType,
        recordManager: typeof row.recordManager === "string"
          ? row.recordManager
          : "",
      }];
    });
  const deduplicated = new Map(rows.map((row) => (
    [`${row.connectionType}:${row.name}`, row]
  )));
  return [...deduplicated.values()].sort((left, right) => {
    const leftManaged = left.recordManager === "CLOUD_SQL_AUTOMATION" ? 0 : 1;
    const rightManaged = right.recordManager === "CLOUD_SQL_AUTOMATION" ? 0 : 1;
    return leftManaged - rightManaged || left.name.localeCompare(right.name);
  });
}

function caMode(
  connectSettings: JsonObject,
  ipConfiguration: JsonObject,
) {
  const values = [connectSettings.serverCaMode, ipConfiguration.serverCaMode]
    .filter((value): value is string => typeof value === "string");
  const normalized = new Set(values.map((value) => (
    value === "CA_MODE_UNSPECIFIED" ? "GOOGLE_MANAGED_INTERNAL_CA" : value
  )));
  if (
    normalized.size !== 1
    || ![...normalized].every((value) => (
      value === "GOOGLE_MANAGED_INTERNAL_CA"
      || value === "GOOGLE_MANAGED_CAS_CA"
      || value === "CUSTOMER_MANAGED_CAS_CA"
    ))
  ) {
    throw new Error("Cloud SQL server CA mode is unavailable or inconsistent");
  }
  return [...normalized][0];
}

function assertSupportedSslMode(ipConfiguration: JsonObject) {
  const sslMode = ipConfiguration.sslMode;
  if (
    sslMode === "TRUSTED_CLIENT_CERTIFICATE_REQUIRED"
    || (
      (sslMode == null || sslMode === "SSL_MODE_UNSPECIFIED")
      && ipConfiguration.requireSsl === true
    )
  ) {
    throw new Error("Cloud SQL requires a client certificate that DopeDB cannot issue");
  }
  if (
    sslMode != null
    && sslMode !== "SSL_MODE_UNSPECIFIED"
    && sslMode !== "ALLOW_UNENCRYPTED_AND_ENCRYPTED"
    && sslMode !== "ENCRYPTED_ONLY"
  ) {
    throw new Error("Cloud SQL returned an unsupported SSL mode");
  }
}

export function gcpConnectionTarget(input: {
  connectSettings: unknown;
  instanceDetails: unknown;
  networkMode: GcpCloudSqlNetworkMode;
}): {
  host: string;
  sslmode: ManagedSslMode;
  tlsServerCaPem: string;
} {
  const connectSettings = object(input.connectSettings);
  const instanceDetails = object(input.instanceDetails);
  const settings = object(instanceDetails?.settings);
  const ipConfiguration = object(settings?.ipConfiguration);
  if (!connectSettings || !instanceDetails || !ipConfiguration) {
    throw new Error("Cloud SQL connection settings are incomplete");
  }
  assertSupportedSslMode(ipConfiguration);
  const selectedCaMode = caMode(connectSettings, ipConfiguration);
  const ca = object(connectSettings.serverCaCert)
    ?? object(instanceDetails.serverCaCert);
  const cert = ca?.cert;
  if (
    typeof cert !== "string"
    || cert.length > 64 * 1_024
    || !cert.startsWith("-----BEGIN CERTIFICATE-----")
    || !cert.trimEnd().endsWith("-----END CERTIFICATE-----")
    || cert.includes("\u0000")
  ) {
    throw new Error("Cloud SQL server CA is unavailable");
  }

  const addressSource = Array.isArray(connectSettings.ipAddresses)
    ? connectSettings.ipAddresses
    : instanceDetails.ipAddresses;
  const addresses = Array.isArray(addressSource)
    ? addressSource.flatMap((value) => {
      const row = object(value);
      return row ? [row] : [];
    })
    : [];
  const targets = dnsTargets(connectSettings, instanceDetails);
  const selectedMode = input.networkMode;

  if (
    selectedCaMode !== "GOOGLE_MANAGED_INTERNAL_CA"
    || selectedMode === "PRIVATE_SERVICE_CONNECT"
  ) {
    let dns = targets.find((item) => item.connectionType === selectedMode)?.name;
    if (!dns && selectedMode === "PRIVATE_SERVICE_CONNECT") {
      const legacyDns = validDnsName(connectSettings.dnsName);
      if (
        connectSettings.pscEnabled === true
        && legacyDns?.endsWith(".sql.goog")
      ) {
        dns = legacyDns;
      }
    }
    if (!dns) {
      throw new Error(
        "Cloud SQL has no instance-scoped DNS name for the selected network",
      );
    }
    const customSans = [
      connectSettings.customSubjectAlternativeNames,
      ipConfiguration.customSubjectAlternativeNames,
    ].flatMap((value) => Array.isArray(value) ? value : [])
      .map(validDnsName)
      .filter((value): value is string => Boolean(value));
    if (
      selectedCaMode === "CUSTOMER_MANAGED_CAS_CA"
      && customSans.length > 0
      && !customSans.includes(dns)
      && !dns.endsWith(".sql.goog")
      && !dns.endsWith(".sql-psa.goog")
      && !dns.endsWith(".sql-psc.goog")
    ) {
      throw new Error("Cloud SQL custom DNS name is not present in the certificate SANs");
    }
    return { host: dns, sslmode: "verify-full", tlsServerCaPem: cert };
  }

  const addressType = selectedMode === "PUBLIC" ? "PRIMARY" : "PRIVATE";
  const address = addresses.find((item) => (
    item.type === addressType && validIpv4(item.ipAddress)
  ));
  if (!address || !validIpv4(address.ipAddress)) {
    throw new Error("Cloud SQL has no address for the selected network");
  }
  return {
    host: address.ipAddress,
    sslmode: "verify-ca",
    tlsServerCaPem: cert,
  };
}

export function normalizeGcpUpstreamStatus(status: number) {
  if (status === 401 || status === 403) return 424;
  return status >= 500 ? 502 : status;
}
