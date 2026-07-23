// Adapter-level checks for GCP error-domain separation and the split between
// ConnectSettings TLS metadata and DatabaseInstance IP configuration.
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  validateGcpCloudSqlCredential,
  validateGcpCloudSqlResource,
} from "./gcp-cloud-sql";
import type { GcpCloudSqlCredential } from "./gcp-cloud-sql-core";

vi.mock("server-only", () => ({}));

const certificate = "-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----";
const credential: GcpCloudSqlCredential = {
  projectId: "sample-project-123",
  projectNumber: "123456789012",
  workloadIdentityPoolId: "vercel-prod",
  workloadIdentityProviderId: "dopedb-app",
  instanceId: "prod-db",
  readServiceAccountEmail:
    "dopedb-read@sample-project-123.iam.gserviceaccount.com",
  writeServiceAccountEmail: null,
  dedicatedServiceAccountsConfirmed: true,
  instanceScopedIamConfirmed: true,
};

function tokenResponse() {
  return Response.json({
    accessToken: "service-account-token",
    expireTime: new Date(Date.now() + 10 * 60_000).toISOString(),
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("GCP Cloud SQL API adapter", () => {
  it("maps upstream GCP authorization failures away from workspace auth 401", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => (
      Response.json({ error: "invalid federation" }, { status: 401 })
    )));

    await expect(validateGcpCloudSqlCredential(
      credential,
      `${"a".repeat(100)}.${"b".repeat(100)}.${"c".repeat(100)}`,
    )).rejects.toMatchObject({
      provider: "gcpCloudSql",
      status: 424,
    });
  });

  it("reads mTLS enforcement from instance details and rejects a direct lease path", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: string | URL | Request) => {
      const url = new URL(String(input));
      if (url.origin === "https://sts.googleapis.com") {
        return Response.json({ access_token: "federated-token" });
      }
      if (url.origin === "https://iamcredentials.googleapis.com") {
        return tokenResponse();
      }
      if (url.pathname.endsWith("/databases")) {
        return Response.json({ items: [{ name: "app" }] });
      }
      if (url.pathname.endsWith("/connectSettings")) {
        return Response.json({
          serverCaMode: "GOOGLE_MANAGED_INTERNAL_CA",
          serverCaCert: { cert: certificate },
          ipAddresses: [{ type: "PRIVATE", ipAddress: "10.0.0.8" }],
        });
      }
      if (url.pathname.endsWith("/users")) {
        return Response.json({
          items: [{
            type: "CLOUD_IAM_SERVICE_ACCOUNT",
            name: credential.readServiceAccountEmail,
          }],
        });
      }
      if (url.pathname.endsWith("/instances/prod-db")) {
        return Response.json({
          name: "prod-db",
          databaseVersion: "POSTGRES_17",
          state: "RUNNABLE",
          settings: {
            databaseFlags: [{
              name: "cloudsql.iam_authentication",
              value: "on",
            }],
            ipConfiguration: {
              sslMode: "TRUSTED_CLIENT_CERTIFICATE_REQUIRED",
              serverCaMode: "GOOGLE_MANAGED_INTERNAL_CA",
            },
          },
        });
      }
      return Response.json({ error: "unexpected request" }, { status: 404 });
    }));

    await expect(validateGcpCloudSqlResource(
      credential,
      `${"a".repeat(100)}.${"b".repeat(100)}.${"c".repeat(100)}`,
      {
        project: credential.projectId,
        instance: credential.instanceId,
        database: "app",
        engine: "postgres",
        networkMode: "PRIVATE_SERVICES_ACCESS",
      },
    )).rejects.toMatchObject({
      provider: "gcpCloudSql",
      status: 409,
      message: expect.stringMatching(/requires a client certificate/),
    });
  });
});
