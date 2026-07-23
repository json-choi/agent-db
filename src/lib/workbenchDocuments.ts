import type { CatalogTable } from "../ipc/types";
import { tableKey } from "./tableRef";

export type WorkbenchDocument =
  | {
      id: string;
      connectionId: string;
      kind: "data";
      table: CatalogTable;
    }
  | {
      id: string;
      connectionId: string;
      kind: "schema" | "activity";
    }
  | {
      id: string;
      connectionId: string;
      kind: "sql";
      draft: string;
    }
  | {
      id: string;
      connectionId: string;
      kind: "documents";
      draft: string | null;
    };

export type QueryDocument = Extract<
  WorkbenchDocument,
  { kind: "sql" | "documents" }
>;

let sequence = 0;

export function stableDocument(
  connectionId: string,
  kind: "schema" | "activity",
): WorkbenchDocument {
  return { id: `${connectionId}:${kind}`, connectionId, kind };
}

export function tableDocument(
  connectionId: string,
  table: CatalogTable,
): WorkbenchDocument {
  return {
    id: `${connectionId}:data:${tableKey(table)}`,
    connectionId,
    kind: "data",
    table,
  };
}

export function queryDocument(
  connectionId: string,
  kind: QueryDocument["kind"],
  draft?: string | null,
): QueryDocument {
  sequence += 1;
  const suffix = `${Date.now().toString(36)}-${sequence.toString(36)}`;
  return kind === "sql"
    ? {
        id: `${connectionId}:sql:${suffix}`,
        connectionId,
        kind,
        draft: draft ?? "SELECT 1;",
      }
    : {
        id: `${connectionId}:documents:${suffix}`,
        connectionId,
        kind,
        draft: draft ?? null,
      };
}

export function supportsDocument(
  document: WorkbenchDocument,
  connectionId: string,
  supportsSql: boolean,
) {
  return (
    document.connectionId === connectionId &&
    (supportsSql ? document.kind !== "documents" : document.kind !== "sql")
  );
}
