import { describe, expect, it } from "vitest";
import {
  NEON_PUBLIC_DATABASE_ESCAPE_SQL,
  NEON_PUBLIC_SCHEMA_CREATE_SQL,
  NEON_PUBLIC_SCHEMA_ESCAPE_SQL,
  createNeonScramVerifier,
  neonIntegrationIdentity,
  neonLeaseRole,
  neonPublicDatabaseBoundaryError,
  neonRoleStatements,
  parseNeonConnectionUri,
  parseNeonResource,
} from "./neon-core";

describe("Neon managed-access normalization", () => {
  it("accepts only a redacted Postgres project selector", () => {
    expect(parseNeonResource({
      project: "quiet-field-123",
      branch: "br-main-123",
      database: "analytics",
      engine: "postgres",
    })).toEqual({
      project: "quiet-field-123",
      branch: "br-main-123",
      database: "analytics",
      engine: "postgres",
      schemas: ["public"],
    });
    expect(() => parseNeonResource({
      project: "quiet-field-123",
      branch: "../other",
      database: "analytics",
      engine: "postgres",
    })).toThrow(/Invalid Neon resource/);
    expect(() => parseNeonResource({
      project: "quiet-field-123",
      branch: "br-main-123",
      database: "analytics",
      engine: "postgres",
      schemas: ["public", "neon_auth"],
    })).toThrow(/schema allowlist/);
  });

  it("builds a bounded role with only a SCRAM verifier in SQL", () => {
    const role = neonLeaseRole("user-12345678", "019c1234-aaaa-bbbb-cccc-123456789012");
    const password = "A".repeat(43);
    const passwordVerifier = createNeonScramVerifier(password, Buffer.alloc(16, 7));
    expect(passwordVerifier).toBe(
      "SCRAM-SHA-256$4096:BwcHBwcHBwcHBwcHBwcHBw=="
        + "$iFIsrb0DTjIHQ//DIXhltU31zrKRa1N4mHhzHvfqFyw="
        + ":iVlPegRkb8mAKxQHjG8LQfyiIBa7O163fgoWbnym+NA=",
    );
    const statements = neonRoleStatements({
      role,
      passwordVerifier,
      expiresAt: new Date(Date.now() + 15 * 60 * 1_000).toISOString(),
      accessMode: "read",
      database: "analytics",
      schemas: ["public", "reporting"],
    });
    expect(role).toMatch(/^dopedb_[a-z0-9]+_[a-z0-9]+$/);
    expect(role.split("_").at(-1)).toHaveLength(32);
    expect(statements.join("\n")).toContain(
      'GRANT SELECT ON ALL TABLES IN SCHEMA "reporting"',
    );
    expect(statements.join("\n")).toContain("default_transaction_read_only = on");
    expect(statements.join("\n")).not.toContain("neon_superuser");
    expect(statements.join("\n")).not.toContain("pg_read_all_data");
    expect(statements.join("\n")).not.toContain("pg_write_all_data");
    expect(statements.join("\n")).not.toContain(password);
    expect(statements.join("\n")).toContain("CONNECTION LIMIT 4");
  });

  it("limits write leases to DML in the selected database schemas", () => {
    const statements = neonRoleStatements({
      role: "dopedb_12345678_abcdef12",
      passwordVerifier: createNeonScramVerifier(
        "B".repeat(43),
        Buffer.alloc(16, 8),
      ),
      expiresAt: new Date(Date.now() + 15 * 60 * 1_000).toISOString(),
      accessMode: "write",
      database: "analytics",
      schemas: ["public"],
    }).join("\n");
    expect(statements).toContain("SELECT, INSERT, UPDATE, DELETE");
    expect(statements).toContain("USAGE, SELECT, UPDATE ON ALL SEQUENCES");
    expect(statements).not.toContain("CREATE ON SCHEMA");
    expect(statements).not.toContain("TRUNCATE");
  });

  it("fails closed when PUBLIC can escape or mutate the schema boundary", () => {
    expect(NEON_PUBLIC_DATABASE_ESCAPE_SQL).toContain(
      "COALESCE(d.datacl, acldefault('d', d.datdba))",
    );
    expect(NEON_PUBLIC_DATABASE_ESCAPE_SQL).toContain(
      "d.datname = current_database()",
    );
    expect(NEON_PUBLIC_DATABASE_ESCAPE_SQL).toContain(
      "acl.grantee = 0",
    );
    expect(NEON_PUBLIC_DATABASE_ESCAPE_SQL).toContain(
      "ARRAY['CREATE', 'TEMPORARY']",
    );
    expect(NEON_PUBLIC_SCHEMA_ESCAPE_SQL).toContain(
      "NOT (n.nspname = ANY($1::text[]))",
    );
    expect(NEON_PUBLIC_SCHEMA_ESCAPE_SQL).toContain(
      "COALESCE(n.nspacl, acldefault('n', n.nspowner))",
    );
    expect(NEON_PUBLIC_SCHEMA_ESCAPE_SQL).toContain(
      "acl.grantee = 0",
    );
    expect(NEON_PUBLIC_SCHEMA_ESCAPE_SQL).toContain(
      "ARRAY['USAGE', 'CREATE']",
    );
    expect(NEON_PUBLIC_SCHEMA_CREATE_SQL).toContain(
      "n.nspname = ANY($1::text[])",
    );
    expect(NEON_PUBLIC_SCHEMA_CREATE_SQL).toContain(
      "acl.grantee = 0 AND acl.privilege_type = 'CREATE'",
    );
  });

  it("explains each unsafe PUBLIC database privilege", () => {
    expect(neonPublicDatabaseBoundaryError(["CREATE"])).toMatch(
      /CREATE privilege.*schema allowlist.*revoke CREATE/i,
    );
    expect(neonPublicDatabaseBoundaryError(["TEMPORARY"])).toMatch(
      /TEMPORARY privilege.*temporary writes.*revoke TEMPORARY/i,
    );
    expect(neonPublicDatabaseBoundaryError(["TEMPORARY", "CREATE"])).toMatch(
      /CREATE and TEMPORARY.*revoke both/i,
    );
    expect(neonPublicDatabaseBoundaryError(["CONNECT"])).toBeNull();
  });

  it("keeps rotated keys with the same project scope on one integration", () => {
    const first = neonIntegrationIdentity(
      { kind: "organization", id: "org-safe-123" },
      ["project-b-123", "project-a-123"],
    );
    const rotated = neonIntegrationIdentity(
      { kind: "organization", id: "org-safe-123" },
      ["project-a-123", "project-b-123"],
    );
    const narrower = neonIntegrationIdentity(
      { kind: "organization", id: "org-safe-123" },
      ["project-a-123"],
    );
    expect(rotated).toEqual(first);
    expect(narrower.externalAccountId).not.toBe(first.externalAccountId);
  });

  it("rejects connection URIs that leave Neon's verified host boundary", () => {
    expect(parseNeonConnectionUri(
      "postgresql://owner:secret@ep-test.us-east-1.aws.neon.tech/app?sslmode=require",
      "app",
      "owner",
    )).toMatchObject({
      host: "ep-test.us-east-1.aws.neon.tech",
      port: 5432,
    });
    expect(() => parseNeonConnectionUri(
      "postgresql://owner:secret@attacker.example/app",
      "app",
      "owner",
    )).toThrow(/Invalid Neon connection URI/);
  });
});
