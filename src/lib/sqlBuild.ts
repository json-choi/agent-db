// Pure SQL + export string builders for the data grid. No React, no IPC — everything
// here is unit-testable. Engine-aware identifier quoting is reused from tableRef;
// literal escaping + WHERE/ORDER BY/paging/DML builders live here so pagination is
// stable (always ordered) and generated writes are injection-safe.
import type { CatalogColumn, CatalogTable, Engine } from "../ipc/types";
import { quoteIdent, tableRef } from "./tableRef";

export interface GridSort {
  col: string;
  dir: "asc" | "desc";
}

// A SQL string literal. Single quotes are doubled for every engine; backslashes are
// doubled for MySQL, which treats "\" as an escape char unless NO_BACKSLASH_ESCAPES.
function sqlLiteral(engine: Engine, value: string | null): string {
  if (value === null) return "NULL";
  let s = value;
  if (engine === "mysql") s = s.replace(/\\/g, "\\\\");
  s = s.replace(/'/g, "''");
  return `'${s}'`;
}

// Word-boundary anchored so it doesn't match "int" embedded in interval/point/etc.
const NUMERIC_RE =
  /\b(?:big|small|tiny|medium)?int(?:eger)?\d*\b|\b(?:big|small)?serial\d*\b|\b(?:numeric|decimal|dec|real|double|float\d*|money|fixed)\b/i;
export function isNumericType(dataType: string): boolean {
  return NUMERIC_RE.test(dataType);
}

// A value literal for a column: NULL keyword, a bare number for numeric columns when
// the text is a valid number, else a quoted string literal (safe for any type).
function sqlValue(engine: Engine, dataType: string, value: string | null): string {
  if (value === null) return "NULL";
  if (isNumericType(dataType)) {
    const t = value.trim();
    // A cleared numeric field is NULL, never the invalid literal '' (PG/MySQL reject it).
    if (t === "") return "NULL";
    if (/^-?\d+(\.\d+)?([eE][+-]?\d+)?$/.test(t)) return t;
  }
  return sqlLiteral(engine, value);
}

// CAST target for text comparison (MySQL uses CHAR; TEXT is invalid there).
function castText(engine: Engine): string {
  return engine === "mysql" ? "CHAR" : "TEXT";
}

// Convert a raw cell value (from QueryResult.rows) to the editor/literal string form.
// null/undefined → SQL NULL; objects → JSON; everything else → String().
export function cellToInput(v: unknown): string | null {
  if (v === null || v === undefined) return null;
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

function typeOf(table: CatalogTable, col: string): string {
  return table.columns.find((c) => c.name === col)?.dataType ?? "text";
}

export function pkColumns(table: CatalogTable): CatalogColumn[] {
  return table.columns.filter((c) => c.pk);
}

// Non-scalar PK types: the grid reads the PK cell as a backend-rendered hex/JSON string
// and sqlValue emits it as a plain quoted literal, which never matches the real bytes/value
// → a WHERE that silently deletes/updates nothing. We block row editing on such PKs rather
// than attempt per-engine binary/composite literal encoding.
const NON_SCALAR_PK_RE = /\b(?:bytea|(?:tiny|medium|long)?blob|(?:var)?binary|json|jsonb|array|composite|record)\b|\[\]/i;
export function hasNonScalarPk(table: CatalogTable): boolean {
  return pkColumns(table).some((c) => NON_SCALAR_PK_RE.test(c.dataType));
}

// One column's filter → a boolean SQL fragment, or null for "no filter":
//   "null" / "not null"  → IS [NOT] NULL
//   "=abc"               → col = <typed literal>            (exact)
//   "abc"                → CAST(col AS text) [I]LIKE '%abc%' (contains, case-insensitive)
function filterClause(engine: Engine, col: CatalogColumn, raw: string): string | null {
  const v = raw.trim();
  if (!v) return null;
  const q = quoteIdent(engine, col.name);
  const low = v.toLowerCase();
  if (low === "null") return `${q} IS NULL`;
  if (low === "not null") return `${q} IS NOT NULL`;
  if (v.startsWith("=")) return `${q} = ${sqlValue(engine, col.dataType, v.slice(1))}`;
  // ponytail: % and _ in the search act as LIKE wildcards (not escaped). Injection is
  // still prevented by literal quoting; add wildcard escaping + ESCAPE if users complain.
  const op = engine === "postgres" ? "ILIKE" : "LIKE";
  return `CAST(${q} AS ${castText(engine)}) ${op} ${sqlLiteral(engine, `%${v}%`)}`;
}

function buildWhere(
  engine: Engine,
  columns: CatalogColumn[],
  filters: Record<string, string>,
): string {
  const parts: string[] = [];
  for (const col of columns) {
    const c = filterClause(engine, col, filters[col.name] ?? "");
    if (c) parts.push(c);
  }
  return parts.join(" AND ");
}

// PK columns as trailing ASC tiebreakers, minus any already named as the sort col, so
// duplicate sort values order deterministically and LIMIT/OFFSET paging can't repeat/skip.
function pkTiebreakers(engine: Engine, table: CatalogTable, exclude?: string): string[] {
  return pkColumns(table)
    .filter((c) => c.name !== exclude)
    .map((c) => quoteIdent(engine, c.name));
}

function buildOrderBy(engine: Engine, table: CatalogTable, sort: GridSort | null): string {
  if (sort) {
    const dir = sort.dir === "desc" ? "DESC" : "ASC";
    const keys = [`${quoteIdent(engine, sort.col)} ${dir}`, ...pkTiebreakers(engine, table, sort.col)];
    return `ORDER BY ${keys.join(", ")}`;
  }
  // Stable default so LIMIT/OFFSET paging can't repeat or skip rows: primary key,
  // falling back to the first column.
  const pk = pkColumns(table);
  const cols = pk.length ? pk : table.columns.slice(0, 1);
  if (!cols.length) return "";
  return `ORDER BY ${cols.map((c) => quoteIdent(engine, c.name)).join(", ")}`;
}

function nn(n: number): number {
  return Math.max(0, Math.floor(n));
}

export function buildPageQuery(
  engine: Engine,
  table: CatalogTable,
  opts: { filters: Record<string, string>; sort: GridSort | null; limit: number; offset: number },
): string {
  const where = buildWhere(engine, table.columns, opts.filters);
  const order = buildOrderBy(engine, table, opts.sort);
  return (
    `SELECT * FROM ${tableRef(engine, table)}` +
    (where ? ` WHERE ${where}` : "") +
    (order ? ` ${order}` : "") +
    ` LIMIT ${nn(opts.limit)} OFFSET ${nn(opts.offset)}`
  );
}

export function buildCountQuery(
  engine: Engine,
  table: CatalogTable,
  filters: Record<string, string>,
): string {
  const where = buildWhere(engine, table.columns, filters);
  return `SELECT COUNT(*) AS n FROM ${tableRef(engine, table)}` + (where ? ` WHERE ${where}` : "");
}

// SET / WHERE assignments preserving column order; only columns present in `values`.
function assignments(engine: Engine, table: CatalogTable, values: Record<string, string | null>): string[] {
  return table.columns
    .filter((c) => c.name in values)
    .map((c) => `${quoteIdent(engine, c.name)} = ${sqlValue(engine, c.dataType, values[c.name])}`);
}

export function buildInsert(engine: Engine, table: CatalogTable, values: Record<string, string | null>): string {
  const cols = table.columns.filter((c) => c.name in values);
  if (!cols.length) throw new Error("INSERT with no columns");
  const idents = cols.map((c) => quoteIdent(engine, c.name)).join(", ");
  const vals = cols.map((c) => sqlValue(engine, c.dataType, values[c.name])).join(", ");
  return `INSERT INTO ${tableRef(engine, table)} (${idents}) VALUES (${vals})`;
}

export function buildUpdate(
  engine: Engine,
  table: CatalogTable,
  pkValues: Record<string, string | null>,
  setValues: Record<string, string | null>,
): string {
  const where = Object.keys(pkValues).map(
    (c) => `${quoteIdent(engine, c)} = ${sqlValue(engine, typeOf(table, c), pkValues[c])}`,
  );
  if (!where.length) throw new Error("refusing UPDATE without a primary key");
  const set = assignments(engine, table, setValues);
  if (!set.length) throw new Error("UPDATE with no changed columns");
  return `UPDATE ${tableRef(engine, table)} SET ${set.join(", ")} WHERE ${where.join(" AND ")}`;
}

export function buildDelete(engine: Engine, table: CatalogTable, pkValues: Record<string, string | null>): string {
  const where = Object.keys(pkValues).map(
    (c) => `${quoteIdent(engine, c)} = ${sqlValue(engine, typeOf(table, c), pkValues[c])}`,
  );
  if (!where.length) throw new Error("refusing DELETE without a primary key");
  return `DELETE FROM ${tableRef(engine, table)} WHERE ${where.join(" AND ")}`;
}

// --- CSV / JSON export (pure) --------------------------------------------------------
// NULL → empty field (CSV) / null (JSON). Fields containing , " or a newline are quoted
// and internal quotes doubled.
function escapeCsvField(v: unknown): string {
  if (v === null || v === undefined) return "";
  const s = typeof v === "object" ? JSON.stringify(v) : String(v);
  return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function toCsv(columns: string[], rows: unknown[][]): string {
  const head = columns.map(escapeCsvField).join(",");
  const body = rows.map((r) => r.map(escapeCsvField).join(",")).join("\n");
  return body ? `${head}\n${body}` : head;
}

export function toJson(columns: string[], rows: unknown[][]): string {
  return JSON.stringify(
    rows.map((r) => Object.fromEntries(columns.map((c, i) => [c, r[i] ?? null]))),
    null,
    2,
  );
}
