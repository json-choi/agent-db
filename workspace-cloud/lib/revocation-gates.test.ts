import { PgDialect } from "drizzle-orm/pg-core";
import type { SQL } from "drizzle-orm";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { executeMock } = vi.hoisted(() => ({
  executeMock: vi.fn(),
}));

vi.mock("server-only", () => ({}));
vi.mock("./db", () => ({
  db: {
    execute: executeMock,
  },
}));

import {
  claimRevocationGate,
  finalizeManagedLeaseIfUnblocked,
  managedLeaseStillDeliverable,
  releaseRevocationGateClaim,
  reserveManagedLeaseIfUnblocked,
} from "./revocation-gates";

const workspaceId = "11111111-1111-4111-8111-111111111111";
const memberId = "22222222-2222-4222-8222-222222222222";
const connectionId = "33333333-3333-4333-8333-333333333333";
const integrationId = "44444444-4444-4444-8444-444444444444";
const leaseId = "55555555-5555-4555-8555-555555555555";

const authority = {
  leaseId,
  organizationId: workspaceId,
  memberId,
  userId: "target-user",
  role: "editor" as const,
  connectionId,
  connectionRevision: 8,
  engine: "postgres" as const,
  integrationId,
  provider: "neon",
  accessMode: "write" as const,
};

const providerLease = {
  host: "db.example.com",
  port: 5432,
  database: "app",
  username: "lease_user",
  password: "one-time-secret",
  sslmode: "verify-full" as const,
  accessMode: "write" as const,
  externalCredentialId: "external-role-id",
  externalCredentialKind: "role" as const,
  expiresAt: "2026-07-23T12:15:00.000Z",
};

function compiledCall(index = 0) {
  const query = executeMock.mock.calls[index]?.[0] as SQL | undefined;
  if (!query) throw new Error(`SQL call ${index} was not executed`);
  const compiled = new PgDialect().sqlToQuery(query);
  return {
    sql: compiled.sql.replace(/\s+/g, " ").trim(),
    params: compiled.params,
  };
}

function orderedLockKeys(params: unknown[]) {
  return params.filter((value): value is string => (
    typeof value === "string"
    && /^(member|connection|integration):/.test(value)
  ));
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.useRealTimers();
  executeMock.mockResolvedValue({ rows: [] });
});

describe("durable revocation gate SQL", () => {
  it("uses a UUID owner as the release CAS token", async () => {
    const pendingAt = new Date("2026-07-23T12:00:00.000Z");
    executeMock
      .mockResolvedValueOnce({ rows: [{ pendingAt }] })
      .mockResolvedValueOnce({ rows: [{ id: memberId }] });

    const claim = await claimRevocationGate({
      kind: "member",
      organizationId: workspaceId,
      memberId,
      userId: "target-user",
    });
    expect(claim).not.toBeNull();
    expect(claim?.claimId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
    );
    await expect(releaseRevocationGateClaim(claim!)).resolves.toBe(true);

    const claimSql = compiledCall(0);
    expect(claimSql.sql).toContain(
      '"revocation_claim_id" = $4::uuid',
    );
    expect(claimSql.sql).toContain(
      'target."revocation_claim_id" IS NULL OR target."revocation_claimed_at" <',
    );
    expect(claimSql.sql).toContain("pg_advisory_xact_lock(hashtextextended(");
    expect(claimSql.params).toContain(claim?.claimId);

    const releaseSql = compiledCall(1);
    expect(releaseSql.sql).toContain('"revocation_claim_id" = $4::uuid');
    expect(releaseSql.sql).toContain('"revocation_claimed_at" = NULL');
    expect(releaseSql.sql).toContain('"revocation_claim_id" = NULL');
    expect(releaseSql.sql).not.toContain(
      '"revocation_pending_at" = NULL',
    );
    expect(releaseSql.params).toContain(claim?.claimId);
  });

  it("increments a connection revision only when the durable pending gate is first opened", async () => {
    const now = new Date("2026-07-23T12:00:00.000Z");
    vi.useFakeTimers();
    vi.setSystemTime(now);
    executeMock.mockResolvedValue({
      rows: [{ pendingAt: now, connectionRevision: "8" }],
    });

    const claim = await claimRevocationGate({
      kind: "connection",
      organizationId: workspaceId,
      connectionId,
    });

    expect(claim).toMatchObject({
      kind: "connection",
      connectionRevision: 8,
      firstPending: true,
    });
    const query = compiledCall();
    expect(query.sql).toContain(
      'SET "revision" = CASE WHEN target."revocation_pending_at" IS NULL '
        + 'THEN target."revision" + 1 ELSE target."revision" END',
    );
    expect(query.sql).toContain(
      '"revocation_pending_at" = COALESCE(target."revocation_pending_at"',
    );
    expect(query.sql).toContain(
      'RETURNING target."revocation_pending_at" AS "pendingAt", '
        + 'target."revision" AS "connectionRevision"',
    );
  });

  it("takes member, connection, and integration advisory locks in one fixed order", async () => {
    executeMock.mockResolvedValue({ rows: [{ status: "blocked" }] });

    await expect(reserveManagedLeaseIfUnblocked(authority)).resolves.toBe("blocked");

    const query = compiledCall();
    expect(query.sql.indexOf("member_gate_lock AS")).toBeLessThan(
      query.sql.indexOf("connection_gate_lock AS"),
    );
    expect(query.sql.indexOf("connection_gate_lock AS")).toBeLessThan(
      query.sql.indexOf("integration_gate_lock AS"),
    );
    expect(query.sql).toContain("FROM member_gate_lock");
    expect(query.sql).toContain("FROM connection_gate_lock");
    expect(orderedLockKeys(query.params).slice(0, 3)).toEqual([
      `member:${workspaceId}:target-user`,
      `connection:${workspaceId}:${connectionId}`,
      `integration:${workspaceId}:${integrationId}`,
    ]);
  });

  it("reserves an authority-checked active slot and pending lease in one statement", async () => {
    executeMock.mockResolvedValue({ rows: [{ status: "reserved" }] });

    await expect(reserveManagedLeaseIfUnblocked(authority)).resolves.toBe("reserved");

    const query = compiledCall().sql;
    expect(executeMock).toHaveBeenCalledOnce();
    expect(query).toContain('authority AS ( SELECT 1 AS "allowed"');
    expect(query).toContain('"workspace_control"."member"."role" =');
    expect(query).toContain('"workspace_control"."member"."role" IN');
    expect(query).toContain(
      '"workspace_control"."workspace_connection"."revision" =',
    );
    expect(query).toContain(
      '"workspace_control"."workspace_provider_integration"."status" = \'active\'',
    );
    expect(query).toContain("generate_series(1, 5)");
    expect(query).toContain('active_lease."active_slot" = slot."value"');
    expect(query).toContain('active_lease."revoked_at" IS NULL');
    expect(query).toContain(
      '"external_credential_kind", "active_slot", "expires_at") SELECT',
    );
    expect(query).toContain(
      'FROM free_slots ORDER BY free_slots."value" ON CONFLICT DO NOTHING',
    );
    expect(query).toContain(
      "WHEN NOT EXISTS (SELECT 1 FROM authority) THEN 'blocked' ELSE 'limit'",
    );
  });

  it("finalizes only the exact live reservation under unchanged authority", async () => {
    executeMock.mockResolvedValue({ rows: [{ id: leaseId }] });

    await expect(
      finalizeManagedLeaseIfUnblocked(authority, providerLease),
    ).resolves.toBe(true);

    const query = compiledCall().sql;
    expect(query).toContain(
      'lease."id" =',
    );
    expect(query).toContain('lease."organization_id" =');
    expect(query).toContain('lease."connection_id" =');
    expect(query).toContain('lease."integration_id" =');
    expect(query).toContain('lease."user_id" =');
    expect(query).toContain('lease."provider" =');
    expect(query).toContain('lease."access_mode" =');
    expect(query).toContain('lease."external_credential_kind" = \'pending\'');
    expect(query).toContain('lease."revoked_at" IS NULL');
    expect(query).toContain('lease."expires_at" > CURRENT_TIMESTAMP');
    expect(query).toContain('"revocation_pending_at" IS NULL');
    expect(query).toContain('"revocation_claim_id" IS NULL');
    expect(query).toContain('"workspace_control"."member"."role" =');
    expect(query).toContain(
      '"workspace_control"."workspace_connection"."revision" =',
    );
  });

  it("delivers only the exact finalized, unrevoked, unexpired lease", async () => {
    executeMock.mockResolvedValue({ rows: [{ id: leaseId }] });

    await expect(
      managedLeaseStillDeliverable(authority, providerLease),
    ).resolves.toBe(true);

    const query = compiledCall().sql;
    expect(query).toContain('lease."external_credential_id" =');
    expect(query).toContain('lease."external_credential_kind" =');
    expect(query).toContain('lease."external_credential_kind" <> \'pending\'');
    expect(query).toContain('lease."expires_at" =');
    expect(query).toContain('lease."expires_at" > CURRENT_TIMESTAMP');
    expect(query).toContain('lease."revoked_at" IS NULL');
    expect(query).toContain('"workspace_control"."member"."role" =');
    expect(query).toContain(
      '"workspace_control"."workspace_connection"."credential_mode" = \'managed\'',
    );
    expect(query).toContain(
      '"workspace_control"."workspace_provider_integration"."revoked_at" IS NULL',
    );
    expect(orderedLockKeys(compiledCall().params).slice(0, 3)).toEqual([
      `member:${workspaceId}:target-user`,
      `connection:${workspaceId}:${connectionId}`,
      `integration:${workspaceId}:${integrationId}`,
    ]);
  });
});
