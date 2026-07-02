// Identifier quoting + table labels, shared by the sidebar explorer and the data view.
import type { CatalogTable, Engine } from "../ipc/types";

export function quoteIdent(engine: Engine, name: string): string {
  if (engine === "mysql") return "`" + name.replace(/`/g, "``") + "`";
  return '"' + name.replace(/"/g, '""') + '"';
}

export function tableRef(engine: Engine, t: CatalogTable): string {
  const q = (n: string) => quoteIdent(engine, n);
  if (engine === "postgres" && t.schema) return `${q(t.schema)}.${q(t.name)}`;
  return q(t.name);
}

export function tableLabel(engine: Engine, t: CatalogTable): string {
  return engine === "postgres" && t.schema && t.schema !== "public"
    ? `${t.schema}.${t.name}`
    : t.name;
}

export function tableKey(t: CatalogTable): string {
  return `${t.schema ?? ""}.${t.name}`;
}
