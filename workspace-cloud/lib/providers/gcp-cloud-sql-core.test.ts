import { describe, expect, it } from "vitest";
import {
  gcpCloudSqlEngine,
  gcpCloudSqlIntegrationIdentity,
  gcpCloudSqlPrincipalClaims,
  gcpConnectionTarget,
  gcpDatabaseUsername,
  normalizeGcpUpstreamStatus,
  parseGcpCloudSqlCredential,
  parseGcpCloudSqlResource,
} from "./gcp-cloud-sql-core";

const certificate = "-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----";
const instanceDetails = (
  sslMode = "ENCRYPTED_ONLY",
  serverCaMode = "GOOGLE_MANAGED_INTERNAL_CA",
) => ({
  settings: {
    ipConfiguration: {
      sslMode,
      serverCaMode,
    },
  },
});

describe("GCP Cloud SQL managed-access normalization", () => {
  it("accepts keyless WIF coordinates and keeps read/write identities separate", () => {
    expect(parseGcpCloudSqlCredential({
      projectId: "sample-project-123",
      projectNumber: "123456789012",
      workloadIdentityPoolId: "vercel-prod",
      workloadIdentityProviderId: "dopedb-app",
      instanceId: "prod-db",
      readServiceAccountEmail:
        "dopedb-read@sample-project-123.iam.gserviceaccount.com",
      writeServiceAccountEmail:
        "dopedb-write@sample-project-123.iam.gserviceaccount.com",
      dedicatedServiceAccountsConfirmed: true,
      instanceScopedIamConfirmed: true,
    })).toMatchObject({
      projectId: "sample-project-123",
      projectNumber: "123456789012",
      instanceId: "prod-db",
    });
    expect(() => parseGcpCloudSqlCredential({
      projectId: "sample-project-123",
      projectNumber: "123456789012",
      workloadIdentityPoolId: "vercel-prod",
      workloadIdentityProviderId: "dopedb-app",
      instanceId: "prod-db",
      readServiceAccountEmail:
        "same-user@sample-project-123.iam.gserviceaccount.com",
      writeServiceAccountEmail:
        "same-user@sample-project-123.iam.gserviceaccount.com",
      dedicatedServiceAccountsConfirmed: true,
      instanceScopedIamConfirmed: true,
    })).toThrow(/Invalid GCP trust configuration/);
    expect(() => parseGcpCloudSqlCredential({
      projectId: "sample-project-123",
      projectNumber: "123456789012",
      workloadIdentityPoolId: "vercel-prod",
      workloadIdentityProviderId: "dopedb-app",
      instanceId: "prod-db",
      readServiceAccountEmail:
        "dopedb-read@sample-project-123.iam.gserviceaccount.com",
      dedicatedServiceAccountsConfirmed: false,
      instanceScopedIamConfirmed: true,
    })).toThrow(/Invalid GCP trust configuration/);
  });

  it("uses immutable WIF, service-account, and instance identity fingerprints", () => {
    const credential = parseGcpCloudSqlCredential({
      projectId: "sample-project-123",
      projectNumber: "123456789012",
      workloadIdentityPoolId: "vercel-prod",
      workloadIdentityProviderId: "dopedb-app",
      instanceId: "prod-db",
      readServiceAccountEmail:
        "dopedb-read@sample-project-123.iam.gserviceaccount.com",
      writeServiceAccountEmail:
        "dopedb-write@sample-project-123.iam.gserviceaccount.com",
      dedicatedServiceAccountsConfirmed: true,
      instanceScopedIamConfirmed: true,
    });
    const identity = gcpCloudSqlIntegrationIdentity(credential);
    expect(identity.externalAccountId).toMatch(
      /^gcp-wif-v1:r[0-9a-f]{64}:w[0-9a-f]{64}:n[0-9a-f]{64}:i[0-9a-f]{64}$/,
    );
    expect(gcpCloudSqlIntegrationIdentity({
      ...credential,
      instanceId: "other-db",
    }).externalAccountId).not.toBe(identity.externalAccountId);
    expect(gcpCloudSqlIntegrationIdentity({
      ...credential,
      workloadIdentityProviderId: "other-provider",
    }).externalAccountId).not.toBe(identity.externalAccountId);

    const claims = gcpCloudSqlPrincipalClaims(identity);
    expect(claims).toEqual([
      {
        principalFingerprint: identity.readPrincipal,
        targetFingerprint: identity.instance,
        accessKind: "read",
      },
      {
        principalFingerprint: identity.writePrincipal,
        targetFingerprint: identity.instance,
        accessKind: "write",
      },
    ]);
    expect(JSON.stringify(claims)).not.toContain("@");
    expect(claims.every((claim) => (
      /^[0-9a-f]{64}$/.test(claim.principalFingerprint)
      && /^[0-9a-f]{64}$/.test(claim.targetFingerprint)
    ))).toBe(true);
  });

  it("maps supported database versions and IAM usernames", () => {
    const email = "dopedb-read@sample-project-123.iam.gserviceaccount.com";
    expect(gcpCloudSqlEngine("POSTGRES_17")).toBe("postgres");
    expect(gcpCloudSqlEngine("MYSQL_8_0")).toBe("mysql");
    expect(gcpCloudSqlEngine("SQLSERVER_2022_STANDARD")).toBeNull();
    expect(gcpDatabaseUsername(email, "postgres"))
      .toBe("dopedb-read@sample-project-123.iam");
    expect(gcpDatabaseUsername(email, "mysql")).toBe("dopedb-read");
  });

  it("uses an exact public or private address with a per-instance CA", () => {
    expect(gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "GOOGLE_MANAGED_INTERNAL_CA",
        serverCaCert: { cert: certificate },
        ipAddresses: [
          { type: "PRIMARY", ipAddress: "203.0.113.10" },
          { type: "PRIVATE", ipAddress: "10.0.0.8" },
        ],
      },
      instanceDetails: instanceDetails(),
      networkMode: "PRIVATE_SERVICES_ACCESS",
    })).toMatchObject({
      host: "10.0.0.8",
      sslmode: "verify-ca",
    });
    expect(gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "CA_MODE_UNSPECIFIED",
        serverCaCert: { cert: certificate },
        ipAddresses: [
          { type: "PRIVATE", ipAddress: "10.0.0.8" },
          { type: "PRIMARY", ipAddress: "203.0.113.10" },
        ],
      },
      instanceDetails: instanceDetails(
        "ENCRYPTED_ONLY",
        "GOOGLE_MANAGED_INTERNAL_CA",
      ),
      networkMode: "PUBLIC",
    })).toMatchObject({
      host: "203.0.113.10",
      sslmode: "verify-ca",
    });
  });

  it("requires an explicit network path for every stored resource", () => {
    expect(() => parseGcpCloudSqlResource({
      project: "sample-project-123",
      instance: "prod-db",
      database: "analytics",
      engine: "postgres",
    })).toThrow(/Invalid GCP Cloud SQL resource/);
  });

  it("requires instance-scoped DNS and verify-full for shared CA networks", () => {
    const connectSettings = {
      serverCaMode: "GOOGLE_MANAGED_CAS_CA",
      serverCaCert: { cert: certificate },
      ipAddresses: [
        { type: "PRIMARY", ipAddress: "203.0.113.10" },
        { type: "PRIVATE", ipAddress: "10.0.0.8" },
      ],
      dnsNames: [
        {
          name: "PUBLIC.UID.REGION.SQL.GOOG.",
          connectionType: "PUBLIC",
          dnsScope: "INSTANCE",
          recordManager: "CLOUD_SQL_AUTOMATION",
        },
        {
          name: "private.uid.region.sql-psa.goog.",
          connectionType: "PRIVATE_SERVICES_ACCESS",
          dnsScope: "INSTANCE",
          recordManager: "CLOUD_SQL_AUTOMATION",
        },
        {
          name: "cluster.global.sql-psa.goog.",
          connectionType: "PRIVATE_SERVICES_ACCESS",
          dnsScope: "CLUSTER",
          recordManager: "CLOUD_SQL_AUTOMATION",
        },
        {
          name: "psc.uid.region.sql-psc.goog.",
          connectionType: "PRIVATE_SERVICE_CONNECT",
          dnsScope: "INSTANCE",
          recordManager: "CLOUD_SQL_AUTOMATION",
        },
      ],
    };
    expect(gcpConnectionTarget({
      connectSettings,
      instanceDetails: instanceDetails("ENCRYPTED_ONLY", "GOOGLE_MANAGED_CAS_CA"),
      networkMode: "PUBLIC",
    })).toMatchObject({
      host: "public.uid.region.sql.goog",
      sslmode: "verify-full",
    });
    expect(gcpConnectionTarget({
      connectSettings,
      instanceDetails: instanceDetails("ENCRYPTED_ONLY", "GOOGLE_MANAGED_CAS_CA"),
      networkMode: "PRIVATE_SERVICES_ACCESS",
    })).toMatchObject({
      host: "private.uid.region.sql-psa.goog",
      sslmode: "verify-full",
    });
    expect(gcpConnectionTarget({
      connectSettings,
      instanceDetails: instanceDetails("ENCRYPTED_ONLY", "GOOGLE_MANAGED_CAS_CA"),
      networkMode: "PRIVATE_SERVICE_CONNECT",
    })).toMatchObject({
      host: "psc.uid.region.sql-psc.goog",
      sslmode: "verify-full",
    });
  });

  it("uses only verified DNS for PSC and customer-managed CA", () => {
    expect(gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "GOOGLE_MANAGED_INTERNAL_CA",
        serverCaCert: { cert: certificate },
        pscEnabled: true,
        dnsName: "psc.uid.region.sql.goog.",
        ipAddresses: [{ type: "PRIMARY", ipAddress: "203.0.113.10" }],
      },
      instanceDetails: instanceDetails(),
      networkMode: "PRIVATE_SERVICE_CONNECT",
    })).toMatchObject({
      host: "psc.uid.region.sql.goog",
      sslmode: "verify-full",
    });
    expect(gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "CUSTOMER_MANAGED_CAS_CA",
        serverCaCert: { cert: certificate },
        customSubjectAlternativeNames: ["db.internal.example"],
        dnsNames: [{
          name: "db.internal.example",
          connectionType: "PRIVATE_SERVICES_ACCESS",
          dnsScope: "INSTANCE",
          recordManager: "CUSTOMER",
        }],
      },
      instanceDetails: {
        settings: {
          ipConfiguration: {
            sslMode: "ENCRYPTED_ONLY",
            serverCaMode: "CUSTOMER_MANAGED_CAS_CA",
            customSubjectAlternativeNames: ["db.internal.example"],
          },
        },
      },
      networkMode: "PRIVATE_SERVICES_ACCESS",
    })).toMatchObject({
      host: "db.internal.example",
      sslmode: "verify-full",
    });
    expect(() => gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "GOOGLE_MANAGED_CAS_CA",
        serverCaCert: { cert: certificate },
        ipAddresses: [{ type: "PRIMARY", ipAddress: "203.0.113.10" }],
      },
      instanceDetails: instanceDetails("ENCRYPTED_ONLY", "GOOGLE_MANAGED_CAS_CA"),
      networkMode: "PUBLIC",
    })).toThrow(/no instance-scoped DNS name/);
  });

  it("fails closed for mTLS, unknown CA modes, and malformed DNS", () => {
    expect(() => gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "GOOGLE_MANAGED_INTERNAL_CA",
        serverCaCert: { cert: certificate },
      },
      instanceDetails: instanceDetails("TRUSTED_CLIENT_CERTIFICATE_REQUIRED"),
      networkMode: "PUBLIC",
    })).toThrow(/requires a client certificate/);
    expect(() => gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "NEW_UNKNOWN_CA",
        serverCaCert: { cert: certificate },
      },
      instanceDetails: instanceDetails("ENCRYPTED_ONLY", "NEW_UNKNOWN_CA"),
      networkMode: "PUBLIC",
    })).toThrow(/CA mode is unavailable or inconsistent/);
    expect(() => gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "GOOGLE_MANAGED_CAS_CA",
        serverCaCert: { cert: certificate },
      },
      instanceDetails: instanceDetails(
        "ENCRYPTED_ONLY",
        "GOOGLE_MANAGED_INTERNAL_CA",
      ),
      networkMode: "PUBLIC",
    })).toThrow(/CA mode is unavailable or inconsistent/);
    expect(() => gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "GOOGLE_MANAGED_INTERNAL_CA",
        serverCaCert: { cert: certificate },
      },
      instanceDetails: instanceDetails("FUTURE_UNKNOWN_MODE"),
      networkMode: "PUBLIC",
    })).toThrow(/unsupported SSL mode/);
    expect(() => gcpConnectionTarget({
      connectSettings: {
        serverCaMode: "GOOGLE_MANAGED_CAS_CA",
        serverCaCert: { cert: certificate },
        dnsNames: [{
          name: "*.sql.goog",
          connectionType: "PUBLIC",
          dnsScope: "INSTANCE",
        }],
      },
      instanceDetails: instanceDetails("ENCRYPTED_ONLY", "GOOGLE_MANAGED_CAS_CA"),
      networkMode: "PUBLIC",
    })).toThrow(/no instance-scoped DNS name/);
  });

  it("rejects a cross-project resource selector", () => {
    expect(parseGcpCloudSqlResource({
      project: "sample-project-123",
      instance: "prod-db",
      database: "analytics",
      engine: "postgres",
      networkMode: "PUBLIC",
    })).toMatchObject({
      instance: "prod-db",
      engine: "postgres",
      networkMode: "PUBLIC",
    });
    expect(() => parseGcpCloudSqlResource({
      project: "https://other",
      instance: "prod-db",
      database: "analytics",
      engine: "postgres",
    })).toThrow(/Invalid GCP Cloud SQL resource/);
    expect(() => parseGcpCloudSqlResource({
      project: "sample-project-123",
      instance: "prod-db",
      database: "analytics",
      engine: "postgres",
      networkMode: "AUTO_PUBLIC",
    })).toThrow(/Invalid GCP Cloud SQL resource/);
  });

  it("normalizes upstream GCP authorization failures away from app auth 401", () => {
    expect(normalizeGcpUpstreamStatus(401)).toBe(424);
    expect(normalizeGcpUpstreamStatus(403)).toBe(424);
    expect(normalizeGcpUpstreamStatus(429)).toBe(429);
    expect(normalizeGcpUpstreamStatus(503)).toBe(502);
  });
});
