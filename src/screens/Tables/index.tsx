// Table data view (DataGrip-style data editor). Server-side pagination with a STABLE
// ORDER BY (primary key) + a real COUNT(*) so pages never repeat/skip rows and the
// total is exact ("rows X-Y of Z"). Column sort and per-column filters go through the
// same sqlBuild helpers. Row edits (insert/update/delete) are generated as SQL and
// routed through ApprovalCard, so the full safety pipeline (classify/preview/approve/
// audit) applies — reads still auto-run and never need approval.
import { useCallback, useEffect, useState } from "react";
import { runSql } from "../../ipc/commands";
import type {
  CatalogTable,
  ConnectionProfile,
  ExecOutcome,
  QueryResult,
  SafetySettings,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import DataGrid from "../../components/DataGrid";
import CellViewer from "../../components/CellViewer";
import RowEditor from "../../components/RowEditor";
import ApprovalCard from "../../components/ApprovalCard";
import { useToast } from "../../components/Toast";
import { tableKey, tableLabel } from "../../lib/tableRef";
import {
  buildCountQuery,
  buildDelete,
  buildPageQuery,
  cellToInput,
  pkColumns,
  toCsv,
  toJson,
  type GridSort,
} from "../../lib/sqlBuild";

const PAGE = 100;

type Editor = { mode: "insert" | "edit" | "duplicate"; initial: Record<string, string | null> };
type CellSel = { value: unknown; column: string };

function download(name: string, text: string, mime: string) {
  const url = URL.createObjectURL(new Blob([text], { type: mime }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}

export default function TableData({
  connection,
  table,
  safety,
}: {
  connection: ConnectionProfile;
  table: CatalogTable;
  safety: SafetySettings;
}) {
  const toast = useToast();
  const engine = connection.engine;
  const [result, setResult] = useState<QueryResult | null>(null);
  const [total, setTotal] = useState<number | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [page, setPage] = useState(0);
  const [sort, setSort] = useState<GridSort | null>(null);
  const [filters, setFilters] = useState<Record<string, string>>({});
  const [selected, setSelected] = useState<number | null>(null);
  const [cellSel, setCellSel] = useState<CellSel | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [prepared, setPrepared] = useState<string | null>(null);

  const pageSize = Math.min(PAGE, safety.maxRows || PAGE);
  const key = tableKey(table);
  const hasPk = pkColumns(table).length > 0;
  const activeFilters = Object.values(filters).filter((v) => v.trim()).length;

  const load = useCallback(
    async (p: number) => {
      setBusy(true);
      setErr(null);
      const pageSql = buildPageQuery(engine, table, {
        filters,
        sort,
        limit: pageSize,
        offset: p * pageSize,
      });
      const countSql = buildCountQuery(engine, table, filters);
      try {
        const [pageOut, countOut] = await Promise.all([
          runSql(connection.id, pageSql, true),
          runSql(connection.id, countSql, true),
        ]);
        setResult(pageOut.result ?? null);
        setPage(p);
        setSelected(null);
        const n = countOut.result?.rows?.[0]?.[0];
        setTotal(n == null ? null : Number(n));
      } catch (e) {
        setErr(errMessage(e));
      } finally {
        setBusy(false);
      }
    },
    [connection.id, engine, table, pageSize, sort, filters],
  );

  // Reset view state whenever the selected table changes.
  useEffect(() => {
    setSort(null);
    setFilters({});
    setSelected(null);
    setCellSel(null);
    setEditor(null);
    setPrepared(null);
  }, [key]);

  // Any query-shape change (table / sort / filters, all folded into `load`'s identity)
  // resets to page 0. Debounced so typing in a filter doesn't fire a query per keystroke.
  useEffect(() => {
    const t = window.setTimeout(() => void load(0), 250);
    return () => window.clearTimeout(t);
  }, [load]);

  const rows = result?.rowCount ?? 0;
  const from = rows === 0 ? 0 : page * pageSize + 1;
  const to = page * pageSize + rows;
  const lastPage = total != null ? Math.max(0, Math.ceil(total / pageSize) - 1) : null;
  const hasPrev = page > 0;
  const hasNext = total != null ? page < (lastPage ?? 0) : rows === pageSize;

  function cycleSort(col: string) {
    setSort((s) =>
      !s || s.col !== col
        ? { col, dir: "asc" }
        : s.dir === "asc"
          ? { col, dir: "desc" }
          : null,
    );
  }

  const selRow = selected != null && result ? result.rows[selected] : null;

  function rowMap(row: unknown[]): Record<string, string | null> {
    const m: Record<string, string | null> = {};
    result!.columns.forEach((c, i) => (m[c] = cellToInput(row[i])));
    return m;
  }

  function openEdit(mode: Editor["mode"]) {
    if (mode === "insert") setEditor({ mode, initial: {} });
    else if (selRow) setEditor({ mode, initial: rowMap(selRow) });
    setPrepared(null);
    setCellSel(null);
  }

  function doDelete() {
    if (!selRow || !result) return;
    const pkVals: Record<string, string | null> = {};
    for (const c of pkColumns(table)) {
      const i = result.columns.indexOf(c.name);
      pkVals[c.name] = i >= 0 ? cellToInput(selRow[i]) : null;
    }
    try {
      setPrepared(buildDelete(engine, table, pkVals));
      setEditor(null);
      setCellSel(null);
    } catch (e) {
      setErr(errMessage(e));
    }
  }

  function copyRow(asJson: boolean) {
    if (!selRow || !result) return;
    const text = asJson
      ? JSON.stringify(
          Object.fromEntries(result.columns.map((c, i) => [c, selRow[i] ?? null])),
          null,
          2,
        )
      : selRow
          .map((v) => (v == null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v)))
          .join("\t");
    void navigator.clipboard.writeText(text);
    toast("Row copied");
  }

  function onWritten(o: ExecOutcome) {
    toast(o.affected != null ? `${o.affected} row(s) written` : "Write committed");
    setPrepared(null);
    setEditor(null);
    void load(page);
  }

  const noPkTitle = "Table has no primary key — row editing disabled";
  const panelOpen = !!prepared || !!editor || !!cellSel;

  return (
    <div className="table-data">
      <div className="table-data-head">
        <strong>{tableLabel(engine, table)}</strong>
        <span className="muted">
          {table.columns.length} cols
          {result && (
            <>
              {" · "}rows {from}–{to}
              {total != null && ` of ${total.toLocaleString()}`}
              {result.truncated ? " (truncated)" : ""} · {result.durationMs} ms
            </>
          )}
        </span>
        <div className="table-pager">
          <button className="btn small" disabled={busy || !hasPrev} onClick={() => void load(0)}>
            « First
          </button>
          <button
            className="btn small"
            disabled={busy || !hasPrev}
            onClick={() => void load(page - 1)}
          >
            ‹ Prev
          </button>
          <span className="muted page-ind">
            Page {page + 1}
            {lastPage != null && ` / ${lastPage + 1}`}
          </span>
          <button
            className="btn small"
            disabled={busy || !hasNext}
            onClick={() => void load(page + 1)}
          >
            Next ›
          </button>
          <button
            className="btn small"
            disabled={busy || lastPage == null || !hasNext}
            onClick={() => lastPage != null && void load(lastPage)}
          >
            Last »
          </button>
          <button className="btn small refresh" disabled={busy} onClick={() => void load(page)}>
            {busy ? "…" : "↻"}
          </button>
        </div>
      </div>

      <div className="grid-toolbar">
        <button
          className="btn small"
          disabled={!hasPk}
          title={hasPk ? undefined : noPkTitle}
          onClick={() => openEdit("insert")}
        >
          + Insert
        </button>
        <button
          className="btn small"
          disabled={!hasPk || selected == null}
          title={hasPk ? undefined : noPkTitle}
          onClick={() => openEdit("edit")}
        >
          Edit
        </button>
        <button
          className="btn small"
          disabled={!hasPk || selected == null}
          title={hasPk ? undefined : noPkTitle}
          onClick={() => openEdit("duplicate")}
        >
          Duplicate
        </button>
        <button
          className="btn small"
          disabled={!hasPk || selected == null}
          title={hasPk ? undefined : noPkTitle}
          onClick={doDelete}
        >
          Delete
        </button>
        <span className="tb-sep" />
        <button className="btn small" disabled={selected == null} onClick={() => copyRow(false)}>
          Copy TSV
        </button>
        <button className="btn small" disabled={selected == null} onClick={() => copyRow(true)}>
          Copy JSON
        </button>
        <button
          className="btn small"
          disabled={!rows}
          onClick={() =>
            result && download(`${table.name}.csv`, toCsv(result.columns, result.rows), "text/csv")
          }
        >
          Export CSV
        </button>
        <button
          className="btn small"
          disabled={!rows}
          onClick={() =>
            result &&
            download(`${table.name}.json`, toJson(result.columns, result.rows), "application/json")
          }
        >
          Export JSON
        </button>
        {activeFilters > 0 && (
          <>
            <span className="tb-sep" />
            <span className="muted">
              {activeFilters} filter{activeFilters > 1 ? "s" : ""}
            </span>
            <button className="btn small" onClick={() => setFilters({})}>
              Clear
            </button>
          </>
        )}
      </div>

      {err && <div className="error">{err}</div>}

      <div className="table-data-body">
        {result ? (
          <DataGrid
            result={result}
            startIndex={page * pageSize}
            sort={sort}
            onSort={cycleSort}
            filters={filters}
            onFilter={(col, value) => setFilters((f) => ({ ...f, [col]: value }))}
            selectedRow={selected}
            onSelectRow={setSelected}
            onCellClick={(value, i, column) => {
              setSelected(i);
              setCellSel({ value, column });
            }}
          />
        ) : (
          !err && <div className={busy ? "muted loading" : "muted"}>{busy ? "Loading rows…" : "No rows."}</div>
        )}

        {panelOpen && (
          <aside className="grid-panel">
            {editor && (
              <RowEditor
                key={`${editor.mode}-${selected}`}
                engine={engine}
                table={table}
                mode={editor.mode}
                initial={editor.initial}
                onSubmit={(sql) => setPrepared(sql)}
                onCancel={() => {
                  setEditor(null);
                  setPrepared(null);
                }}
              />
            )}
            {prepared && (
              <ApprovalCard
                key={prepared}
                connectionId={connection.id}
                engine={engine}
                sql={prepared}
                safety={safety}
                onExecuted={onWritten}
                onReject={() => setPrepared(null)}
              />
            )}
            {cellSel && !editor && !prepared && (
              <CellViewer
                value={cellSel.value}
                column={cellSel.column}
                onClose={() => setCellSel(null)}
              />
            )}
          </aside>
        )}
      </div>
    </div>
  );
}
