import type { Catalog, CatalogTable, ConnectionProfile } from "../ipc/types";
import { tableKey } from "./tableRef";

const ENV_ORDER: Record<string, number> = {
  prod: 0,
  staging: 1,
  dev: 2,
};

export interface SchemaConnectionGroup {
  key: string;
  label: string;
  connections: ConnectionProfile[];
}

export type ConnectionSection =
  | { kind: "group"; group: SchemaConnectionGroup }
  | { kind: "single"; connection: ConnectionProfile };

export interface TableSchemaDiff {
  key: string;
  added: boolean;
  missing: boolean;
  relationChanged: boolean;
  addedColumns: string[];
  missingColumns: string[];
  changedColumns: string[];
}

export interface SchemaDiffSummary {
  addedTables: CatalogTable[];
  missingTables: CatalogTable[];
  changedTables: CatalogTable[];
  addedColumns: number;
  missingColumns: number;
  changedColumns: number;
  relationChangedTables: number;
  total: number;
  tableDiffs: Record<string, TableSchemaDiff>;
}

export function schemaGroupLabel(conn: ConnectionProfile): string {
  return conn.schemaGroup?.trim() ?? "";
}

export function schemaGroupKey(conn: ConnectionProfile): string | null {
  const label = schemaGroupLabel(conn);
  return label ? label.toLocaleLowerCase() : null;
}

export function buildConnectionSections(connections: ConnectionProfile[]): ConnectionSection[] {
  const groups = new Map<
    string,
    SchemaConnectionGroup & { firstIndex: number }
  >();

  connections.forEach((conn, index) => {
    const key = schemaGroupKey(conn);
    if (!key) return;
    const existing = groups.get(key);
    if (existing) {
      existing.connections.push(conn);
    } else {
      groups.set(key, {
        key,
        label: schemaGroupLabel(conn),
        connections: [conn],
        firstIndex: index,
      });
    }
  });

  const seenGroups = new Set<string>();
  const sections: Array<ConnectionSection & { index: number }> = [];
  connections.forEach((conn, index) => {
    const key = schemaGroupKey(conn);
    if (!key) {
      sections.push({ kind: "single", connection: conn, index });
      return;
    }
    if (seenGroups.has(key)) return;
    seenGroups.add(key);
    const group = groups.get(key);
    if (!group) return;
    const ordered = [...group.connections].sort(compareConnectionsInGroup);
    sections.push({
      kind: "group",
      group: { key: group.key, label: group.label, connections: ordered },
      index: group.firstIndex,
    });
  });

  return sections
    .sort((a, b) => a.index - b.index)
    .map(({ index: _index, ...section }) => section);
}

export function compareCatalogs(current: Catalog, baseline: Catalog): SchemaDiffSummary {
  const currentTables = new Map(current.tables.map((table) => [tableKey(table), table]));
  const baselineTables = new Map(
    baseline.tables.map((table) => [tableKey(table), table]),
  );
  const addedTables: CatalogTable[] = [];
  const missingTables: CatalogTable[] = [];
  const changedTables: CatalogTable[] = [];
  const tableDiffs: Record<string, TableSchemaDiff> = {};
  let addedColumns = 0;
  let missingColumns = 0;
  let changedColumns = 0;
  let relationChangedTables = 0;

  for (const [key, table] of currentTables) {
    const base = baselineTables.get(key);
    if (!base) {
      addedTables.push(table);
      tableDiffs[key] = emptyTableDiff(key, { added: true });
      continue;
    }

    const diff = diffTable(table, base);
    if (hasTableDiff(diff)) {
      tableDiffs[key] = diff;
      changedTables.push(table);
      addedColumns += diff.addedColumns.length;
      missingColumns += diff.missingColumns.length;
      changedColumns += diff.changedColumns.length;
      if (diff.relationChanged) relationChangedTables += 1;
    }
  }

  for (const [key, table] of baselineTables) {
    if (currentTables.has(key)) continue;
    missingTables.push(table);
    tableDiffs[key] = emptyTableDiff(key, { missing: true });
  }

  const total =
    addedTables.length +
    missingTables.length +
    addedColumns +
    missingColumns +
    changedColumns +
    relationChangedTables;

  return {
    addedTables,
    missingTables,
    changedTables,
    addedColumns,
    missingColumns,
    changedColumns,
    relationChangedTables,
    total,
    tableDiffs,
  };
}

export function tableDiffTone(
  diff: TableSchemaDiff | undefined,
): "added" | "missing" | "changed" | "mixed" | null {
  if (!diff) return null;
  if (diff.added) return "added";
  if (diff.missing) return "missing";
  const hasAdd = diff.addedColumns.length > 0;
  const hasMissing = diff.missingColumns.length > 0;
  const hasChange = diff.changedColumns.length > 0 || diff.relationChanged;
  const kinds = [hasAdd, hasMissing, hasChange].filter(Boolean).length;
  if (kinds > 1) return "mixed";
  if (hasAdd) return "added";
  if (hasMissing) return "missing";
  if (hasChange) return "changed";
  return null;
}

export function diffCounts(diff: SchemaDiffSummary) {
  return {
    added: diff.addedTables.length + diff.addedColumns,
    missing: diff.missingTables.length + diff.missingColumns,
    changed: diff.changedColumns + diff.relationChangedTables,
  };
}

function compareConnectionsInGroup(a: ConnectionProfile, b: ConnectionProfile) {
  const envA = ENV_ORDER[a.env ?? ""] ?? 9;
  const envB = ENV_ORDER[b.env ?? ""] ?? 9;
  if (envA !== envB) return envA - envB;
  return (a.name || a.database).localeCompare(b.name || b.database);
}

function emptyTableDiff(
  key: string,
  flags: Partial<Pick<TableSchemaDiff, "added" | "missing">> = {},
): TableSchemaDiff {
  return {
    key,
    added: flags.added ?? false,
    missing: flags.missing ?? false,
    relationChanged: false,
    addedColumns: [],
    missingColumns: [],
    changedColumns: [],
  };
}

function diffTable(current: CatalogTable, baseline: CatalogTable): TableSchemaDiff {
  const diff = emptyTableDiff(tableKey(current));
  const currentColumns = new Map(current.columns.map((column) => [column.name, column]));
  const baselineColumns = new Map(
    baseline.columns.map((column) => [column.name, column]),
  );

  for (const [name, column] of currentColumns) {
    const base = baselineColumns.get(name);
    if (!base) {
      diff.addedColumns.push(name);
    } else if (columnSignature(column) !== columnSignature(base)) {
      diff.changedColumns.push(name);
    }
  }

  for (const name of baselineColumns.keys()) {
    if (!currentColumns.has(name)) diff.missingColumns.push(name);
  }

  diff.relationChanged = relationSignature(current) !== relationSignature(baseline);
  return diff;
}

function hasTableDiff(diff: TableSchemaDiff): boolean {
  return (
    diff.added ||
    diff.missing ||
    diff.relationChanged ||
    diff.addedColumns.length > 0 ||
    diff.missingColumns.length > 0 ||
    diff.changedColumns.length > 0
  );
}

function columnSignature(column: CatalogTable["columns"][number]): string {
  return [
    column.dataType.trim().toLocaleLowerCase(),
    column.nullable ? "null" : "not-null",
    column.pk ? "pk" : "no-pk",
  ].join("|");
}

function relationSignature(table: CatalogTable): string {
  const indexes = table.indexes
    .map((idx) => `${idx.name}:${idx.unique ? "u" : "n"}:${idx.columns.join(",")}`)
    .sort()
    .join(";");
  const foreignKeys = table.foreignKeys
    .map(
      (fk) =>
        `${fk.column}->${fk.referencesSchema ?? ""}.${fk.referencesTable}.${fk.referencesColumn}`,
    )
    .sort()
    .join(";");
  return [table.kind, indexes, foreignKeys].join("|");
}
