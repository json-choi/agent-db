// Pure SQL + export string builders for the data grid. No React, no IPC — everything
// here is unit-testable. Engine-aware identifier quoting is reused from tableRef;
// literal escaping + WHERE/ORDER BY/paging/DML builders live here so pagination is
// stable (always ordered) and generated writes are injection-safe.
import type { CatalogColumn, CatalogTable, Engine } from "../ipc/types";
import { quoteIdent, tableRef } from "./tableRef";

export { quoteIdent };

export interface GridSort {
  col: string;
  dir: "asc" | "desc";
}

// A SQL string literal. Single quotes are doubled for every engine; backslashes are
// doubled for MySQL, which treats "\" as an escape char unless NO_BACKSLASH_ESCAPES.
export function sqlLiteral(engine: Engine, value: string | null): string {
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
export function sqlValue(engine: Engine, dataType: string, value: string | null): string {
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

export function buildWhere(
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

export function buildOrderBy(engine: Engine, table: CatalogTable, sort: GridSort | null): string {
  if (sort) {
    return `ORDER BY ${quoteIdent(engine, sort.col)} ${sort.dir === "desc" ? "DESC" : "ASC"}`;
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
export function escapeCsvField(v: unknown): string {
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

// ponytail: repo has no test runner; this is the runnable check. Verify with esbuild:
//   pnpm exec esbuild src/lib/sqlBuild.ts --bundle --format=esm --outfile=/tmp/sb.mjs \
//     && node --input-type=module -e "import('/tmp/sb.mjs').then(m=>{m.__selfTest();console.log('ok')})"
export function __selfTest(): void {
  const a = (cond: boolean, msg: string) => {
    if (!cond) throw new Error("selfTest failed: " + msg);
  };
  const t: CatalogTable = {
    schema: null,
    name: "users",
    kind: "table",
    foreignKeys: [],
    indexes: [],
    rowEstimate: null,
    columns: [
      { name: "id", dataType: "integer", nullable: false, pk: true },
      { name: "name", dataType: "text", nullable: false, pk: false },
    ],
  };
  a(sqlLiteral("postgres", "a'b") === "'a''b'", "pg quote doubling");
  a(sqlLiteral("mysql", "a\\b") === "'a\\\\b'", "mysql backslash doubling");
  a(sqlLiteral("postgres", "a\\b") === "'a\\b'", "pg keeps backslash");
  a(sqlValue("postgres", "integer", "5") === "5", "numeric raw");
  a(sqlValue("postgres", "integer", "5); DROP") === "'5); DROP'", "numeric fallback quoted");
  a(sqlValue("postgres", "text", null) === "NULL", "null keyword");
  a(isNumericType("integer") && isNumericType("bigint") && isNumericType("int4"), "int types numeric");
  a(!isNumericType("interval") && !isNumericType("point"), "interval/point not numeric");
  a(sqlValue("postgres", "integer", "") === "NULL", "empty numeric -> NULL");
  a(sqlValue("postgres", "interval", "5") === "'5'", "interval quoted not bare");
  a(sqlValue("postgres", "text", "") === "''", "empty text stays ''");
  a(buildOrderBy("postgres", t, null) === 'ORDER BY "id"', "default order by pk");
  a(buildOrderBy("postgres", t, { col: "name", dir: "desc" }) === 'ORDER BY "name" DESC', "sort desc");
  const noPk: CatalogTable = { ...t, columns: [{ name: "x", dataType: "text", nullable: true, pk: false }] };
  a(buildOrderBy("mysql", noPk, null) === "ORDER BY `x`", "fallback to first col");
  a(buildWhere("postgres", t.columns, { name: "=bob" }) === `"name" = 'bob'`, "exact filter");
  a(buildWhere("postgres", t.columns, { id: "null" }) === `"id" IS NULL`, "null filter");
  a(buildWhere("postgres", t.columns, { id: "not null" }) === `"id" IS NOT NULL`, "not null filter");
  a(buildWhere("postgres", t.columns, { name: "ob" }) === `CAST("name" AS TEXT) ILIKE '%ob%'`, "contains pg");
  a(buildWhere("mysql", t.columns, { name: "ob" }) === "CAST(`name` AS CHAR) LIKE '%ob%'", "contains mysql");
  a(buildWhere("postgres", t.columns, { id: "1", name: "" }).indexOf(" AND ") === -1, "single filter no AND");
  a(
    buildPageQuery("postgres", t, { filters: {}, sort: null, limit: 10, offset: 20 }) ===
      'SELECT * FROM "users" ORDER BY "id" LIMIT 10 OFFSET 20',
    "page query ordered",
  );
  a(buildCountQuery("postgres", t, {}) === 'SELECT COUNT(*) AS n FROM "users"', "count query");
  a(buildInsert("postgres", t, { id: "1", name: "x" }) === `INSERT INTO "users" ("id", "name") VALUES (1, 'x')`, "insert");
  a(buildUpdate("postgres", t, { id: "1" }, { name: "y" }) === `UPDATE "users" SET "name" = 'y' WHERE "id" = 1`, "update");
  a(buildDelete("postgres", t, { id: "1" }) === `DELETE FROM "users" WHERE "id" = 1`, "delete");
  let threw = false;
  try {
    buildDelete("postgres", t, {});
  } catch {
    threw = true;
  }
  a(threw, "delete without pk throws");
  a(escapeCsvField(null) === "", "csv null empty");
  a(escapeCsvField('a,"b') === '"a,""b"', "csv quote+comma");
  a(toCsv(["a", "b"], [[1, null]]) === "a,b\n1,", "csv rows, null empty");
  a(toJson(["a"], [[null]]) === '[\n  {\n    "a": null\n  }\n]', "json null");
}
