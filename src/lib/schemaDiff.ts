// Pure schema-group construction and catalog comparison. The diff model feeds both
// compact sidebar summaries and the full group comparison workspace.
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

export type SchemaDiffStatus = "added" | "missing" | "changed" | "same";
export type SchemaObjectType = "table" | "view" | "column" | "index" | "foreignKey";

export interface SchemaObjectDiff {
  id: string;
  tableKey: string;
  objectType: SchemaObjectType;
  path: string;
  label: string;
  status: Exclude<SchemaDiffStatus, "same">;
  baselineValue: string;
  targetValue: string;
}

export interface TableSchemaDiff {
  key: string;
  added: boolean;
  missing: boolean;
  relationChanged: boolean;
  addedColumns: string[];
  missingColumns: string[];
  changedColumns: string[];
  objectDiffs: SchemaObjectDiff[];
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
  objects: SchemaObjectDiff[];
}

function schemaGroupLabel(conn: ConnectionProfile): string {
  return conn.schemaGroup?.trim() ?? "";
}

function schemaGroupKey(conn: ConnectionProfile): string | null {
  const label = schemaGroupLabel(conn);
  return label ? label.toLocaleLowerCase() : null;
}

export function buildConnectionSections(connections: ConnectionProfile[]): ConnectionSection[] {
  const groups = new Map<string, SchemaConnectionGroup & { firstIndex: number }>();

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

export function schemaGroupIsCompatible(group: SchemaConnectionGroup): boolean {
  const engine = group.connections[0]?.engine;
  return !!engine && group.connections.every((connection) => connection.engine === engine);
}

export function defaultSchemaBaseline(group: SchemaConnectionGroup): ConnectionProfile | null {
  return (
    group.connections.find((connection) => connection.env === "prod") ??
    group.connections[0] ??
    null
  );
}

export function compareCatalogs(current: Catalog, baseline: Catalog): SchemaDiffSummary {
  const currentTables = new Map(current.tables.map((table) => [tableKey(table), table]));
  const baselineTables = new Map(baseline.tables.map((table) => [tableKey(table), table]));
  const addedTables: CatalogTable[] = [];
  const missingTables: CatalogTable[] = [];
  const changedTables: CatalogTable[] = [];
  const tableDiffs: Record<string, TableSchemaDiff> = {};
  const objects: SchemaObjectDiff[] = [];
  let addedColumns = 0;
  let missingColumns = 0;
  let changedColumns = 0;
  let relationChangedTables = 0;

  for (const [key, table] of currentTables) {
    const base = baselineTables.get(key);
    if (!base) {
      addedTables.push(table);
      const object = tableObjectDiff(table, "added");
      const diff = emptyTableDiff(key, { added: true });
      diff.objectDiffs.push(object);
      tableDiffs[key] = diff;
      objects.push(object);
      continue;
    }

    const diff = diffTable(table, base);
    if (hasTableDiff(diff)) {
      tableDiffs[key] = diff;
      changedTables.push(table);
      objects.push(...diff.objectDiffs);
      addedColumns += diff.addedColumns.length;
      missingColumns += diff.missingColumns.length;
      changedColumns += diff.changedColumns.length;
      if (diff.relationChanged) relationChangedTables += 1;
    }
  }

  for (const [key, table] of baselineTables) {
    if (currentTables.has(key)) continue;
    missingTables.push(table);
    const object = tableObjectDiff(table, "missing");
    const diff = emptyTableDiff(key, { missing: true });
    diff.objectDiffs.push(object);
    tableDiffs[key] = diff;
    objects.push(object);
  }

  objects.sort((a, b) => a.path.localeCompare(b.path) || a.objectType.localeCompare(b.objectType));

  return {
    addedTables,
    missingTables,
    changedTables,
    addedColumns,
    missingColumns,
    changedColumns,
    relationChangedTables,
    total: objects.length,
    tableDiffs,
    objects,
  };
}

export function tableDiffTone(
  diff: TableSchemaDiff | undefined,
): "added" | "missing" | "changed" | "mixed" | null {
  if (!diff) return null;
  if (diff.added) return "added";
  if (diff.missing) return "missing";
  const hasAdd = diff.objectDiffs.some((object) => object.status === "added");
  const hasMissing = diff.objectDiffs.some((object) => object.status === "missing");
  const hasChange = diff.objectDiffs.some((object) => object.status === "changed");
  const kinds = [hasAdd, hasMissing, hasChange].filter(Boolean).length;
  if (kinds > 1) return "mixed";
  if (hasAdd) return "added";
  if (hasMissing) return "missing";
  if (hasChange) return "changed";
  return null;
}

export function diffCounts(diff: SchemaDiffSummary) {
  return {
    added: diff.objects.filter((object) => object.status === "added").length,
    missing: diff.objects.filter((object) => object.status === "missing").length,
    changed: diff.objects.filter((object) => object.status === "changed").length,
  };
}

export function orderTablesBySchemaDiff(
  tables: CatalogTable[],
  diff: SchemaDiffSummary | null,
): CatalogTable[] {
  if (!diff) return tables;
  return tables
    .map((table, index) => ({
      table,
      index,
      changed: tableDiffTone(diff.tableDiffs[tableKey(table)]) !== null,
    }))
    .sort((a, b) => Number(b.changed) - Number(a.changed) || a.index - b.index)
    .map(({ table }) => table);
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
    objectDiffs: [],
  };
}

function tableObjectDiff(
  table: CatalogTable,
  status: "added" | "missing",
): SchemaObjectDiff {
  const key = tableKey(table);
  return {
    id: `${key}:${table.kind}`,
    tableKey: key,
    objectType: table.kind === "view" ? "view" : "table",
    path: key,
    label: table.name,
    status,
    baselineValue: status === "missing" ? table.kind : "—",
    targetValue: status === "added" ? table.kind : "—",
  };
}

function diffTable(current: CatalogTable, baseline: CatalogTable): TableSchemaDiff {
  const key = tableKey(current);
  const diff = emptyTableDiff(key);
  const currentColumns = new Map(current.columns.map((column) => [column.name, column]));
  const baselineColumns = new Map(baseline.columns.map((column) => [column.name, column]));

  if (current.kind !== baseline.kind) {
    diff.objectDiffs.push({
      id: `${key}:kind`,
      tableKey: key,
      objectType: current.kind === "view" ? "view" : "table",
      path: key,
      label: current.name,
      status: "changed",
      baselineValue: baseline.kind,
      targetValue: current.kind,
    });
  }

  for (const [name, column] of currentColumns) {
    const base = baselineColumns.get(name);
    if (!base) {
      diff.addedColumns.push(name);
      diff.objectDiffs.push(objectDiff(key, "column", name, "added", "—", columnValue(column)));
    } else if (columnSignature(column) !== columnSignature(base)) {
      diff.changedColumns.push(name);
      diff.objectDiffs.push(
        objectDiff(key, "column", name, "changed", columnValue(base), columnValue(column)),
      );
    }
  }

  for (const [name, column] of baselineColumns) {
    if (currentColumns.has(name)) continue;
    diff.missingColumns.push(name);
    diff.objectDiffs.push(objectDiff(key, "column", name, "missing", columnValue(column), "—"));
  }

  appendNamedObjectDiffs(
    diff.objectDiffs,
    key,
    "index",
    new Map(current.indexes.map((index) => [index.name, indexValue(index)])),
    new Map(baseline.indexes.map((index) => [index.name, indexValue(index)])),
  );
  appendForeignKeyDiffs(diff.objectDiffs, key, current.foreignKeys, baseline.foreignKeys);

  diff.relationChanged = diff.objectDiffs.some(
    (object) => object.objectType === "index" || object.objectType === "foreignKey",
  );
  return diff;
}

function appendNamedObjectDiffs(
  objects: SchemaObjectDiff[],
  table: string,
  objectType: "index" | "foreignKey",
  current: Map<string, string>,
  baseline: Map<string, string>,
) {
  for (const [name, value] of current) {
    const base = baseline.get(name);
    if (base == null) {
      objects.push(objectDiff(table, objectType, name, "added", "—", value));
    } else if (base !== value) {
      objects.push(objectDiff(table, objectType, name, "changed", base, value));
    }
  }
  for (const [name, value] of baseline) {
    if (!current.has(name)) {
      objects.push(objectDiff(table, objectType, name, "missing", value, "—"));
    }
  }
}

function appendForeignKeyDiffs(
  objects: SchemaObjectDiff[],
  table: string,
  currentForeignKeys: CatalogTable["foreignKeys"],
  baselineForeignKeys: CatalogTable["foreignKeys"],
) {
  const current = groupForeignKeysByColumn(currentForeignKeys);
  const baseline = groupForeignKeysByColumn(baselineForeignKeys);
  const columns = new Set([...current.keys(), ...baseline.keys()]);

  for (const column of columns) {
    const currentValues = current.get(column) ?? [];
    const baselineValues = baseline.get(column) ?? [];
    if (
      currentValues.length === 1 &&
      baselineValues.length === 1 &&
      currentValues[0] !== baselineValues[0]
    ) {
      objects.push(
        foreignKeyObjectDiff(
          table,
          column,
          "changed",
          baselineValues[0],
          currentValues[0],
          "changed",
        ),
      );
      continue;
    }

    const added = unmatchedValues(currentValues, baselineValues);
    const missing = unmatchedValues(baselineValues, currentValues);
    added.forEach((value, index) => {
      objects.push(
        foreignKeyObjectDiff(table, column, "added", "—", value, `added:${index}:${value}`),
      );
    });
    missing.forEach((value, index) => {
      objects.push(
        foreignKeyObjectDiff(table, column, "missing", value, "—", `missing:${index}:${value}`),
      );
    });
  }
}

function groupForeignKeysByColumn(
  foreignKeys: CatalogTable["foreignKeys"],
): Map<string, string[]> {
  const grouped = new Map<string, string[]>();
  for (const foreignKey of foreignKeys) {
    const values = grouped.get(foreignKey.column) ?? [];
    values.push(foreignKeyValue(foreignKey));
    grouped.set(foreignKey.column, values);
  }
  for (const values of grouped.values()) values.sort();
  return grouped;
}

function unmatchedValues(source: string[], comparison: string[]): string[] {
  const remaining = [...comparison];
  return source.filter((value) => {
    const match = remaining.indexOf(value);
    if (match < 0) return true;
    remaining.splice(match, 1);
    return false;
  });
}

function foreignKeyObjectDiff(
  table: string,
  column: string,
  status: "added" | "missing" | "changed",
  baselineValue: string,
  targetValue: string,
  identity: string,
): SchemaObjectDiff {
  return {
    id: `${table}:foreignKey:${column}:${identity}`,
    tableKey: table,
    objectType: "foreignKey",
    path: `${table}.${column}`,
    label: column,
    status,
    baselineValue,
    targetValue,
  };
}

function objectDiff(
  table: string,
  objectType: "column" | "index" | "foreignKey",
  name: string,
  status: "added" | "missing" | "changed",
  baselineValue: string,
  targetValue: string,
): SchemaObjectDiff {
  return {
    id: `${table}:${objectType}:${name}`,
    tableKey: table,
    objectType,
    path: `${table}.${name}`,
    label: name,
    status,
    baselineValue,
    targetValue,
  };
}

function hasTableDiff(diff: TableSchemaDiff): boolean {
  return diff.added || diff.missing || diff.objectDiffs.length > 0;
}

function columnSignature(column: CatalogTable["columns"][number]): string {
  return [
    column.dataType.trim().toLocaleLowerCase(),
    column.nullable ? "null" : "not-null",
    column.pk ? "pk" : "no-pk",
  ].join("|");
}

function columnValue(column: CatalogTable["columns"][number]): string {
  return [column.dataType, column.nullable ? "NULL" : "NOT NULL", column.pk ? "PK" : ""]
    .filter(Boolean)
    .join(" · ");
}

function indexValue(index: CatalogTable["indexes"][number]): string {
  return `${index.unique ? "UNIQUE " : ""}(${index.columns.join(", ")})`;
}

function foreignKeyValue(foreignKey: CatalogTable["foreignKeys"][number]): string {
  const schema = foreignKey.referencesSchema ? `${foreignKey.referencesSchema}.` : "";
  return `${foreignKey.column} → ${schema}${foreignKey.referencesTable}.${foreignKey.referencesColumn}`;
}
