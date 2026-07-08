import { useEffect, useMemo, useState } from "react";
import { getCatalog } from "../../ipc/commands";
import type { Catalog, CatalogTable, ConnectionProfile } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { tableKey, tableLabel } from "../../lib/tableRef";
import { useI18n } from "../../lib/i18n";
import "./schema.css";

const NODE_W = 280;
const NODE_H = 172;
const GAP_X = 56;
const GAP_Y = 54;

type Relationship = {
  id: string;
  fromKey: string;
  toKey: string;
  fromTable: CatalogTable;
  toTable: CatalogTable;
  fromColumn: string;
  toColumn: string;
};

function matchesFilter(table: CatalogTable, filter: string) {
  if (!filter) return true;
  const haystack = [
    table.schema,
    table.name,
    ...table.columns.flatMap((c) => [c.name, c.dataType]),
    ...table.indexes.flatMap((i) => [i.name, ...i.columns]),
    ...table.foreignKeys.flatMap((f) => [
      f.column,
      f.referencesTable,
      f.referencesColumn,
      f.referencesSchema,
    ]),
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return haystack.includes(filter);
}

function findReferencedTable(catalog: Catalog, fk: CatalogTable["foreignKeys"][number]) {
  return catalog.tables.find((candidate) => {
    if (candidate.name !== fk.referencesTable) return false;
    if (fk.referencesSchema && candidate.schema !== fk.referencesSchema) return false;
    return true;
  });
}

function relationshipsFor(catalog: Catalog): Relationship[] {
  const relationships: Relationship[] = [];
  for (const table of catalog.tables) {
    for (const fk of table.foreignKeys) {
      const target = findReferencedTable(catalog, fk);
      if (!target) continue;
      relationships.push({
        id: `${tableKey(table)}:${fk.column}->${tableKey(target)}:${fk.referencesColumn}`,
        fromKey: tableKey(table),
        toKey: tableKey(target),
        fromTable: table,
        toTable: target,
        fromColumn: fk.column,
        toColumn: fk.referencesColumn,
      });
    }
  }
  return relationships;
}

function labelFor(table: CatalogTable) {
  return table.schema ? `${table.schema}.${table.name}` : table.name;
}

function pathBetween(
  from: { x: number; y: number },
  to: { x: number; y: number },
) {
  const fromCenterX = from.x + NODE_W / 2;
  const fromCenterY = from.y + NODE_H / 2;
  const toCenterX = to.x + NODE_W / 2;
  const toCenterY = to.y + NODE_H / 2;

  if (from.x === to.x && from.y === to.y) {
    const x = from.x + NODE_W - 20;
    const y = from.y + 38;
    return `M ${x} ${y} C ${x + 62} ${y - 54}, ${x + 68} ${y + 86}, ${x} ${y + 78}`;
  }

  if (Math.abs(toCenterX - fromCenterX) >= Math.abs(toCenterY - fromCenterY)) {
    const leftToRight = toCenterX > fromCenterX;
    const sx = leftToRight ? from.x + NODE_W : from.x;
    const sy = fromCenterY;
    const tx = leftToRight ? to.x : to.x + NODE_W;
    const ty = toCenterY;
    const dx = Math.max(60, Math.abs(tx - sx) * 0.45);
    const c1x = sx + (leftToRight ? dx : -dx);
    const c2x = tx + (leftToRight ? -dx : dx);
    return `M ${sx} ${sy} C ${c1x} ${sy}, ${c2x} ${ty}, ${tx} ${ty}`;
  }

  const topToBottom = toCenterY > fromCenterY;
  const sx = fromCenterX;
  const sy = topToBottom ? from.y + NODE_H : from.y;
  const tx = toCenterX;
  const ty = topToBottom ? to.y : to.y + NODE_H;
  const dy = Math.max(50, Math.abs(ty - sy) * 0.45);
  const c1y = sy + (topToBottom ? dy : -dy);
  const c2y = ty + (topToBottom ? -dy : dy);
  return `M ${sx} ${sy} C ${sx} ${c1y}, ${tx} ${c2y}, ${tx} ${ty}`;
}

export default function SchemaExplorer({
  connection,
  selectedTable,
  onOpenTable,
}: {
  connection: ConnectionProfile;
  selectedTable: CatalogTable | null;
  onOpenTable: (table: CatalogTable) => void;
}) {
  const { t } = useI18n();
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [selectedKey, setSelectedKey] = useState<string | null>(
    selectedTable ? tableKey(selectedTable) : null,
  );

  useEffect(() => {
    let alive = true;
    setCatalog(null);
    setError(null);
    getCatalog(connection.id)
      .then((next) => {
        if (!alive) return;
        setCatalog(next);
        const preferred = selectedTable ? tableKey(selectedTable) : null;
        setSelectedKey((current) => {
          if (preferred && next.tables.some((table) => tableKey(table) === preferred)) {
            return preferred;
          }
          if (current && next.tables.some((table) => tableKey(table) === current)) {
            return current;
          }
          return next.tables[0] ? tableKey(next.tables[0]) : null;
        });
      })
      .catch((e) => alive && setError(errMessage(e)));
    return () => {
      alive = false;
    };
  }, [connection.id, selectedTable]);

  const allRelationships = useMemo(
    () => (catalog ? relationshipsFor(catalog) : []),
    [catalog],
  );
  const normalizedFilter = filter.trim().toLowerCase();
  const tables = useMemo(
    () =>
      catalog
        ? catalog.tables.filter((table) => matchesFilter(table, normalizedFilter))
        : [],
    [catalog, normalizedFilter],
  );
  const visibleKeys = new Set(tables.map(tableKey));
  const relationships = allRelationships.filter(
    (r) => visibleKeys.has(r.fromKey) && visibleKeys.has(r.toKey),
  );
  const selected =
    tables.find((table) => tableKey(table) === selectedKey) ??
    catalog?.tables.find((table) => tableKey(table) === selectedKey) ??
    null;

  const cols = tables.length <= 1 ? 1 : Math.min(3, Math.ceil(Math.sqrt(tables.length)));
  const rows = Math.max(1, Math.ceil(tables.length / cols));
  const width = cols * NODE_W + (cols - 1) * GAP_X;
  const height = rows * NODE_H + (rows - 1) * GAP_Y;
  const positions = new Map(
    tables.map((table, index) => [
      tableKey(table),
      {
        x: (index % cols) * (NODE_W + GAP_X),
        y: Math.floor(index / cols) * (NODE_H + GAP_Y),
      },
    ]),
  );

  if (error) return <div className="screen schema-screen error">{error}</div>;
  if (!catalog) return <div className="screen schema-screen muted loading">{t("schema.loading")}</div>;

  return (
    <div className="screen schema-screen">
      <div className="schema-head">
        <div>
          <h2>{t("schema.title")}</h2>
          <p className="muted">
            {t("schema.tableCount", { count: catalog.tables.length })} ·{" "}
            {t("schema.fkCount", { count: allRelationships.length })}
          </p>
        </div>
        <input
          className="schema-filter"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder={t("schema.filterPlaceholder")}
          type="search"
        />
      </div>

      {catalog.tables.length === 0 ? (
        <div className="placeholder muted">{t("schema.empty")}</div>
      ) : tables.length === 0 ? (
        <div className="placeholder muted">{t("schema.noMatch")}</div>
      ) : (
        <div className="schema-layout">
          <div className="schema-canvas-wrap">
            <div
              className="schema-canvas"
              style={{ width: `${width}px`, height: `${height}px` }}
            >
              <svg
                className="schema-lines"
                width={width}
                height={height}
                viewBox={`0 0 ${width} ${height}`}
                aria-hidden="true"
              >
                <defs>
                  <marker
                    id="schema-arrow"
                    markerWidth="10"
                    markerHeight="10"
                    refX="8"
                    refY="5"
                    orient="auto"
                    markerUnits="strokeWidth"
                  >
                    <path d="M 0 0 L 10 5 L 0 10 z" />
                  </marker>
                </defs>
                {relationships.map((rel) => {
                  const from = positions.get(rel.fromKey);
                  const to = positions.get(rel.toKey);
                  if (!from || !to) return null;
                  const highlighted =
                    selectedKey === rel.fromKey || selectedKey === rel.toKey;
                  return (
                    <path
                      key={rel.id}
                      className={highlighted ? "schema-link active" : "schema-link"}
                      d={pathBetween(from, to)}
                      markerEnd="url(#schema-arrow)"
                    />
                  );
                })}
              </svg>

              {tables.map((table) => {
                const pos = positions.get(tableKey(table))!;
                const key = tableKey(table);
                const pk = table.columns.filter((c) => c.pk);
                const fkCols = new Set(table.foreignKeys.map((fk) => fk.column));
                return (
                  <button
                    key={key}
                    className={selectedKey === key ? "schema-node selected" : "schema-node"}
                    style={{ left: `${pos.x}px`, top: `${pos.y}px` }}
                    onClick={() => setSelectedKey(key)}
                  >
                    <span className="schema-node-head">
                      <strong>{labelFor(table)}</strong>
                      {table.kind === "view" && <em>{t("schema.view")}</em>}
                    </span>
                    <span className="schema-node-meta">
                      {t("schema.columnCount", { count: table.columns.length })}
                      {pk.length > 0 && ` · ${t("schema.pk")} ${pk.map((c) => c.name).join(", ")}`}
                    </span>
                    <span className="schema-cols">
                      {table.columns.slice(0, 5).map((column) => (
                        <span key={column.name}>
                          <code>{column.name}</code>
                          {column.pk && <b>{t("schema.pk")}</b>}
                          {fkCols.has(column.name) && <b>FK</b>}
                        </span>
                      ))}
                      {table.columns.length > 5 && (
                        <span className="muted">+{table.columns.length - 5}</span>
                      )}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>

          <aside className="schema-inspector">
            {selected ? (
              <>
                <div className="schema-inspector-head">
                  <div>
                    <h3>{tableLabel(connection.engine, selected)}</h3>
                    <p className="muted">
                      {t("schema.columnCount", { count: selected.columns.length })}
                    </p>
                  </div>
                  <button className="btn small" onClick={() => onOpenTable(selected)}>
                    {t("schema.openData")}
                  </button>
                </div>
                <div className="schema-detail-list">
                  {selected.columns.map((column) => (
                    <div className="schema-detail-row" key={column.name}>
                      <span>
                        <code>{column.name}</code>
                        {column.pk && <b>{t("schema.pk")}</b>}
                      </span>
                      <em>{column.dataType}</em>
                    </div>
                  ))}
                </div>
                <h3>{t("schema.relationships")}</h3>
                {allRelationships.filter(
                  (rel) => rel.fromKey === tableKey(selected) || rel.toKey === tableKey(selected),
                ).length ? (
                  <ul className="schema-rel-list">
                    {allRelationships
                      .filter(
                        (rel) =>
                          rel.fromKey === tableKey(selected) ||
                          rel.toKey === tableKey(selected),
                      )
                      .map((rel) => (
                        <li key={rel.id}>
                          {t("schema.relationshipText", {
                            fromTable: labelFor(rel.fromTable),
                            fromColumn: rel.fromColumn,
                            toTable: labelFor(rel.toTable),
                            toColumn: rel.toColumn,
                          })}
                        </li>
                      ))}
                  </ul>
                ) : (
                  <p className="muted">{t("schema.noForeignKeys")}</p>
                )}
                <h3>{t("schema.indexes")}</h3>
                {selected.indexes.length ? (
                  <ul className="schema-rel-list">
                    {selected.indexes.map((index) => (
                      <li key={index.name}>
                        {index.name}: {index.columns.join(", ")}
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="muted">{t("common.none")}</p>
                )}
              </>
            ) : (
              <p className="muted">{t("schema.selectTable")}</p>
            )}
          </aside>
        </div>
      )}
    </div>
  );
}
