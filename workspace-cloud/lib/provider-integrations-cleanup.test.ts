import { PgDialect } from "drizzle-orm/pg-core";
import type { SQL } from "drizzle-orm";
import { afterEach, describe, expect, it, vi } from "vitest";

const { executeMock } = vi.hoisted(() => ({
  executeMock: vi.fn(async (_query: unknown) => ({ rows: [] })),
}));

vi.mock("server-only", () => ({}));
vi.mock("./db", () => ({
  db: {
    execute: executeMock,
  },
}));

import {
  cleanupExpiredManagedLeases,
  managedLeaseAuthorityMatches,
  managedLeaseCleanupRetryDelayMs,
} from "./provider-integrations";

afterEach(() => {
  executeMock.mockClear();
});

function compiledClaimSql() {
  const query = executeMock.mock.calls[0]?.[0] as SQL | undefined;
  if (!query) throw new Error("cleanup claim query was not executed");
  return new PgDialect().sqlToQuery(query).sql.replace(/\s+/g, " ").trim();
}

describe("durable managed-lease cleanup", () => {
  it("claims due rows atomically, recovers stale claims, and interleaves tenants", async () => {
    await expect(cleanupExpiredManagedLeases({ limit: 10 })).resolves.toEqual({
      scanned: 0,
      revoked: 0,
      deferred: 0,
    });

    const query = compiledClaimSql();
    expect(query).toContain("ROW_NUMBER() OVER ( PARTITION BY ranked_lease.\"organization_id\"");
    expect(query).toContain(
      "ORDER BY ranked.\"cleanup_attempts\" ASC, ranked.tenant_rank ASC",
    );
    expect(query).toContain("\"cleanup_next_attempt_at\" <= CURRENT_TIMESTAMP");
    expect(query).toContain("\"cleanup_claimed_at\" IS NULL");
    expect(query).toContain("FOR UPDATE OF lease SKIP LOCKED");
    expect(query).toContain(
      "SET \"cleanup_claimed_at\" = CURRENT_TIMESTAMP, "
        + "\"cleanup_attempts\" = lease.\"cleanup_attempts\" + 1",
    );
    expect(executeMock).toHaveBeenCalledTimes(1);
  });

  it("uses bounded exponential backoff so permanent failures yield to fresh rows", () => {
    expect(managedLeaseCleanupRetryDelayMs(1)).toBe(60_000);
    expect(managedLeaseCleanupRetryDelayMs(2)).toBe(120_000);
    expect(managedLeaseCleanupRetryDelayMs(3)).toBe(240_000);
    expect(managedLeaseCleanupRetryDelayMs(7)).toBe(3_600_000);
    expect(managedLeaseCleanupRetryDelayMs(100)).toBe(3_600_000);
    expect(() => managedLeaseCleanupRetryDelayMs(0)).toThrow(/attempt/);
  });

  it("uses a separate SKIP LOCKED claim statement for concurrent workers", async () => {
    await Promise.all([
      cleanupExpiredManagedLeases({ limit: 3 }),
      cleanupExpiredManagedLeases({ limit: 3 }),
    ]);
    expect(executeMock).toHaveBeenCalledTimes(2);
    for (const [query] of executeMock.mock.calls) {
      const statement = new PgDialect()
        .sqlToQuery(query as SQL)
        .sql
        .replace(/\s+/g, " ");
      expect(statement).toContain("FOR UPDATE OF lease SKIP LOCKED");
      expect(statement).toContain("UPDATE \"workspace_control\".\"workspace_credential_lease\"");
    }
  });

  it("fails closed when a lease crosses a connection or integration tenant", () => {
    const authority = {
      leaseOrganizationId: "workspace-a",
      connectionOrganizationId: "workspace-a",
      leaseIntegrationId: "integration-a",
      connectionIntegrationId: "integration-a",
      integrationOrganizationId: "workspace-a",
      leaseProvider: "neon",
      integrationProvider: "neon",
    };
    expect(managedLeaseAuthorityMatches(authority)).toBe(true);
    expect(managedLeaseAuthorityMatches({
      ...authority,
      connectionOrganizationId: "workspace-b",
    })).toBe(false);
    expect(managedLeaseAuthorityMatches({
      ...authority,
      connectionIntegrationId: "integration-b",
    })).toBe(false);
    expect(managedLeaseAuthorityMatches({
      ...authority,
      integrationOrganizationId: "workspace-b",
    })).toBe(false);
  });
});
