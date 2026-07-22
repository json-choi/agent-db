import { describe, expect, it } from "vitest";
import type { Catalog, CatalogTable, ConnectionProfile, Engine } from "../ipc/types";
import {
  buildConnectionSections,
  compareCatalogs,
  defaultSchemaBaseline,
  diffCounts,
  orderTablesBySchemaDiff,
  schemaGroupIsCompatible,
  type SchemaConnectionGroup,
} from "./schemaDiff";

function connection(
  id: string,
  options: Partial<ConnectionProfile> = {},
): ConnectionProfile {
  return {
    id,
    name: id,
    engine: "postgres",
    provider: "generic",
    driverId: null,
    host: "localhost",
    port: 5432,
    database: id,
    username: "tester",
    sslmode: "disable",
    extraParams: {},
    readonlyDefault: true,
    allowWrites: false,
    secretRef: null,
    env: null,
    schemaGroup: null,
    workspaceAccess: "local",
    ...options,
  };
}

function table(
  name: string,
  options: Partial<CatalogTable> = {},
): CatalogTable {
  return {
    schema: "public",
    name,
    kind: "table",
    columns: [],
    foreignKeys: [],
    indexes: [],
    rowEstimate: null,
    ...options,
  };
}

function catalog(...tables: CatalogTable[]): Catalog {
  return { tables };
}

describe("schema connection groups", () => {
  it("groups labels case-insensitively and orders prod, staging, then dev", () => {
    const dev = connection("dev", { env: "dev", schemaGroup: "Core" });
    const prod = connection("prod", { env: "prod", schemaGroup: "core" });
    const staging = connection("staging", { env: "staging", schemaGroup: "CORE" });
    const standalone = connection("standalone");

    const sections = buildConnectionSections([dev, standalone, staging, prod]);

    expect(sections).toHaveLength(2);
    expect(sections[0]).toMatchObject({
      kind: "group",
      group: {
        key: "core",
        label: "Core",
        connections: [{ id: "prod" }, { id: "staging" }, { id: "dev" }],
      },
    });
    expect(sections[1]).toMatchObject({ kind: "single", connection: { id: "standalone" } });
  });

  it("chooses prod as the default baseline and rejects mixed engines", () => {
    const group: SchemaConnectionGroup = {
      key: "core",
      label: "Core",
      connections: [
        connection("dev", { env: "dev", schemaGroup: "Core" }),
        connection("prod", { env: "prod", schemaGroup: "Core" }),
      ],
    };

    expect(defaultSchemaBaseline(group)?.id).toBe("prod");
    expect(schemaGroupIsCompatible(group)).toBe(true);

    const mysql = connection("mysql", {
      engine: "mysql" as Engine,
      schemaGroup: "Core",
    });
    expect(schemaGroupIsCompatible({ ...group, connections: [...group.connections, mysql] })).toBe(
      false,
    );
  });
});

describe("compareCatalogs", () => {
  it("reverses added and missing when the comparison direction changes", () => {
    const baseline = catalog(table("users"));
    const target = catalog(table("orders"));

    const forward = compareCatalogs(target, baseline);
    const reverse = compareCatalogs(baseline, target);

    expect(forward.objects.map(({ path, status }) => ({ path, status }))).toEqual([
      { path: "public.orders", status: "added" },
      { path: "public.users", status: "missing" },
    ]);
    expect(reverse.objects.map(({ path, status }) => ({ path, status }))).toEqual([
      { path: "public.orders", status: "missing" },
      { path: "public.users", status: "added" },
    ]);
  });

  it("reports column values before and after a change", () => {
    const baseline = catalog(
      table("users", {
        columns: [{ name: "email", dataType: "varchar(100)", nullable: true, pk: false }],
      }),
    );
    const target = catalog(
      table("users", {
        columns: [{ name: "email", dataType: "text", nullable: false, pk: false }],
      }),
    );

    const diff = compareCatalogs(target, baseline);

    expect(diff.objects).toEqual([
      expect.objectContaining({
        objectType: "column",
        path: "public.users.email",
        status: "changed",
        baselineValue: "varchar(100) · NULL",
        targetValue: "text · NOT NULL",
      }),
    ]);
    expect(diffCounts(diff)).toEqual({ added: 0, missing: 0, changed: 1 });
  });

  it("reports index and foreign-key additions, removals, and changes", () => {
    const baseline = catalog(
      table("orders", {
        indexes: [
          { name: "orders_user_idx", columns: ["user_id"], unique: false },
          { name: "orders_old_idx", columns: ["old_code"], unique: false },
        ],
        foreignKeys: [
          {
            column: "user_id",
            referencesSchema: "public",
            referencesTable: "users",
            referencesColumn: "id",
          },
          {
            column: "legacy_id",
            referencesSchema: "legacy",
            referencesTable: "records",
            referencesColumn: "id",
          },
        ],
      }),
    );
    const target = catalog(
      table("orders", {
        indexes: [
          { name: "orders_user_idx", columns: ["user_id", "created_at"], unique: true },
          { name: "orders_new_idx", columns: ["status"], unique: false },
        ],
        foreignKeys: [
          {
            column: "user_id",
            referencesSchema: "accounts",
            referencesTable: "users",
            referencesColumn: "id",
          },
          {
            column: "team_id",
            referencesSchema: "public",
            referencesTable: "teams",
            referencesColumn: "id",
          },
        ],
      }),
    );

    const diff = compareCatalogs(target, baseline);
    const changes = new Map(diff.objects.map((object) => [`${object.objectType}:${object.label}`, object.status]));

    expect(changes).toEqual(
      new Map([
        ["foreignKey:legacy_id", "missing"],
        ["foreignKey:team_id", "added"],
        ["foreignKey:user_id", "changed"],
        ["index:orders_new_idx", "added"],
        ["index:orders_old_idx", "missing"],
        ["index:orders_user_idx", "changed"],
      ]),
    );
    expect(diff.relationChangedTables).toBe(1);
    expect(diffCounts(diff)).toEqual({ added: 2, missing: 2, changed: 2 });
  });

  it("preserves multiple foreign keys that share one source column", () => {
    const sharedUserForeignKey = {
      column: "subject_id",
      referencesSchema: "public",
      referencesTable: "users",
      referencesColumn: "id",
    };
    const baseline = catalog(
      table("events", {
        foreignKeys: [
          sharedUserForeignKey,
          {
            column: "subject_id",
            referencesSchema: "public",
            referencesTable: "teams",
            referencesColumn: "id",
          },
        ],
      }),
    );
    const target = catalog(table("events", { foreignKeys: [sharedUserForeignKey] }));

    const foreignKeyDiffs = compareCatalogs(target, baseline).objects.filter(
      (object) => object.objectType === "foreignKey",
    );

    expect(foreignKeyDiffs).toEqual([
      expect.objectContaining({
        label: "subject_id",
        status: "missing",
        baselineValue: "subject_id → public.teams.id",
        targetValue: "—",
      }),
    ]);
  });

  it("distinguishes a table from a view kind change", () => {
    const baseline = catalog(table("active_users"));
    const target = catalog(table("active_users", { kind: "view" }));

    expect(compareCatalogs(target, baseline).objects).toEqual([
      expect.objectContaining({
        objectType: "view",
        status: "changed",
        baselineValue: "table",
        targetValue: "view",
      }),
    ]);
  });

  it("moves tables with differences first while preserving their relative order", () => {
    const alpha = table("alpha");
    const beta = table("beta", {
      columns: [{ name: "name", dataType: "text", nullable: false, pk: false }],
    });
    const gamma = table("gamma");
    const baseline = catalog(
      alpha,
      table("beta", {
        columns: [{ name: "name", dataType: "varchar(30)", nullable: false, pk: false }],
      }),
    );
    const diff = compareCatalogs(catalog(alpha, beta, gamma), baseline);

    expect(orderTablesBySchemaDiff([alpha, beta, gamma], diff).map(({ name }) => name)).toEqual([
      "beta",
      "gamma",
      "alpha",
    ]);
  });
});
