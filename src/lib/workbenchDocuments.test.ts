import { describe, expect, it } from "vitest";
import type { CatalogTable } from "../ipc/types";
import {
  queryDocument,
  stableDocument,
  supportsDocument,
  tableDocument,
} from "./workbenchDocuments";

const table: CatalogTable = {
  schema: "public",
  name: "users",
  kind: "table",
  columns: [],
  foreignKeys: [],
  indexes: [],
  rowEstimate: null,
};

describe("workbench documents", () => {
  it("uses stable ids for singleton resources and table documents", () => {
    expect(stableDocument("db-1", "schema").id).toBe("db-1:schema");
    expect(stableDocument("db-1", "activity").id).toBe("db-1:activity");
    expect(tableDocument("db-1", table).id).toBe("db-1:data:public.users");
  });

  it("creates independent query documents with safe defaults", () => {
    const first = queryDocument("db-1", "sql");
    const second = queryDocument("db-1", "sql", "select * from users");
    const documentQuery = queryDocument("db-1", "documents");

    expect(first.id).not.toBe(second.id);
    expect(first.draft).toBe("SELECT 1;");
    expect(second.draft).toBe("select * from users");
    expect(documentQuery.draft).toBeNull();
  });

  it("keeps documents scoped to the selected connection and engine capability", () => {
    const sql = queryDocument("db-1", "sql");
    const documents = queryDocument("db-1", "documents");

    expect(supportsDocument(sql, "db-1", true)).toBe(true);
    expect(supportsDocument(sql, "db-1", false)).toBe(false);
    expect(supportsDocument(documents, "db-1", false)).toBe(true);
    expect(supportsDocument(documents, "db-2", false)).toBe(false);
  });
});
