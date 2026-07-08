// Table data view (DataGrip-style data editor). Server-side pagination with a STABLE
// ORDER BY (primary key) + a real COUNT(*) so pages never repeat/skip rows and the
// total is exact ("rows X-Y of Z"). Column sort and per-column filters go through the
// same sqlBuild helpers. Row edits (insert/update/delete) are generated as SQL and
// routed through ApprovalCard, so the full safety pipeline (classify/preview/approve/
// audit) applies — reads still auto-run and never need approval.
import { useCallback, useEffect, useRef, useState } from "react";
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
import { Icon } from "../../components/Icon";
import CellViewer from "../../components/CellViewer";
import RowEditor, { type RowEditorSubmission } from "../../components/RowEditor";
import ApprovalCard from "../../components/ApprovalCard";
import { useToast } from "../../components/Toast";
import { tableKey, tableLabel } from "../../lib/tableRef";
import { downloadCsv, downloadJson, stamp } from "../../lib/export";
import { useI18n } from "../../lib/i18n";
import {
  buildCountQuery,
  buildDelete,
  buildPageQuery,
  cellToInput,
  hasNonScalarPk,
  pkColumns,
  type GridSort,
} from "../../lib/sqlBuild";

const PAGE = 100;

type Editor = { mode: "insert" | "edit" | "duplicate"; initial: Record<string, string | null> };
type CellSel = { value: unknown; column: string };
type PreparedWrite = {
  sql: string;
  rationale?: string;
  collapseSql?: boolean;
};

export default function TableData({
  connection,
  table,
  safety,
}: {
  connection: ConnectionProfile;
  table: CatalogTable;
  safety: SafetySettings;
}) {
  const { t } = useI18n();
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
  const [prepared, setPrepared] = useState<PreparedWrite | null>(null);
  // Readable confirm gate for DELETE: PK pairs of the target row, mirroring how
  // insert/edit/duplicate pass through RowEditor before arming the ApprovalCard.
  const [pendingDelete, setPendingDelete] = useState<Record<string, string | null> | null>(null);
  // Reads run on a POOLED connection, so overlapping loads (fast filter typing, table
  // switch) can resolve out of order. Each load takes a ticket; only the latest one paints.
  const reqId = useRef(0);
  const [structure, setStructure] = useState(false);

  const pageSize = Math.min(PAGE, safety.maxRows || PAGE);
  const key = tableKey(table);
  // Row editing needs a PK we can match on. No PK, or a PK whose rendered cell value can't
  // round-trip to a literal (binary/json/array/composite), both disable it — same as noPk.
  const nonScalarPk = hasNonScalarPk(table);
  const canEdit = pkColumns(table).length > 0 && !nonScalarPk;
  const activeFilters = Object.values(filters).filter((v) => v.trim()).length;

  const load = useCallback(
    async (p: number) => {
      const my = ++reqId.current;
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
        if (my !== reqId.current) return; // a newer load already superseded us — drop stale rows
        setResult(pageOut.result ?? null);
        setPage(p);
        setSelected(null);
        setCellSel(null); // the old cell viewer points at rows that just went away
        const n = countOut.result?.rows?.[0]?.[0];
        setTotal(n == null ? null : Number(n));
      } catch (e) {
        if (my !== reqId.current) return; // stale error from a superseded load
        setErr(errMessage(e));
      } finally {
        if (my === reqId.current) setBusy(false);
      }
    },
    [connection.id, engine, table, pageSize, sort, filters],
  );

  // Reset view state whenever the selected table changes.
  useEffect(() => {
    reqId.current++; // invalidate any in-flight load from the previous table
    setSort(null);
    setFilters({});
    setSelected(null);
    setCellSel(null);
    setEditor(null);
    setPrepared(null);
    setPendingDelete(null);
    setStructure(false);
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

  // Open a readable confirm (PK pairs) instead of jumping straight to the ApprovalCard —
  // Delete sits next to Duplicate, so a mis-click shouldn't be one approval from a wipe.
  function doDelete() {
    if (!selRow || !result) return;
    const pkVals: Record<string, string | null> = {};
    for (const c of pkColumns(table)) {
      const i = result.columns.indexOf(c.name);
      pkVals[c.name] = i >= 0 ? cellToInput(selRow[i]) : null;
    }
    setPendingDelete(pkVals);
    setEditor(null);
    setCellSel(null);
    setPrepared(null); // drop any abandoned write card so only the delete confirm is live
  }

  // Confirmed: build the DELETE and arm the ApprovalCard.
  function armDelete() {
    if (!pendingDelete) return;
    try {
      setPrepared({
        sql: buildDelete(engine, table, pendingDelete),
        rationale: `Delete selected row from ${table.name}.`,
        collapseSql: true,
      });
      setPendingDelete(null);
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
    toast(t("tables.copyRow"));
  }

  function onWritten(o: ExecOutcome) {
    // A committed write that matched no rows is not a success — flag it, don't green-light it.
    if (o.affected === 0) toast(t("tables.noRowsWritten"), "error");
    else toast(o.affected != null ? t("tables.rowsWritten", { count: o.affected }) : t("tables.writeCommitted"));
    setPrepared(null);
    setEditor(null);
    setPendingDelete(null);
    void load(page);
  }

  const noEditTitle = nonScalarPk
    ? t("tables.nonScalarPk")
    : t("tables.noTablePk");
  const panelOpen = !!prepared || !!editor || !!cellSel || !!pendingDelete;

  return (
    <div className="table-data">
      <div className="table-data-head">
        <strong>{tableLabel(engine, table)}</strong>
        <span className="muted">
          {t("tables.cols", { count: table.columns.length })}
          {result && (
            <>
              {" · "}
              {total != null
                ? t("tables.rowRangeTotal", {
                    from,
                    to,
                    total: total.toLocaleString(),
                  })
                : t("tables.rowRange", { from, to })}
              {result.truncated ? " (truncated)" : ""} · {result.durationMs} ms
            </>
          )}
        </span>
        <div className="table-pager">
          <button className="btn small" disabled={busy || !hasPrev} onClick={() => void load(0)}>
            « {t("common.first")}
          </button>
          <button
            className="btn small"
            disabled={busy || !hasPrev}
            onClick={() => void load(page - 1)}
          >
            ‹ {t("common.prev")}
          </button>
          <span className="muted page-ind">
            {t("tables.page", { page: page + 1 })}
            {lastPage != null && ` / ${lastPage + 1}`}
          </span>
          <button
            className="btn small"
            disabled={busy || !hasNext}
            onClick={() => void load(page + 1)}
          >
            {t("common.next")} ›
          </button>
          <button
            className="btn small"
            disabled={busy || lastPage == null || !hasNext}
            onClick={() => lastPage != null && void load(lastPage)}
          >
            {t("tables.last")} »
          </button>
          <button
            className="btn small refresh"
            disabled={busy}
            aria-label={t("common.refresh")}
            title={t("common.refresh")}
            onClick={() => void load(page)}
          >
            {busy ? "…" : <Icon name="refresh" />}
          </button>
          <button
            className="btn small"
            aria-expanded={structure}
            title={t("tables.structureTitle")}
            onClick={() => setStructure((s) => !s)}
          >
            {t("tables.structure")}
          </button>
        </div>
      </div>

      {/* Introspected metadata already on the prop — no backend call. Collapsed by default. */}
      {structure && (
        <div className="table-structure">
          <table className="struct-table">
            <thead>
              <tr>
                <th>{t("tables.column")}</th>
                <th>{t("tables.type")}</th>
                <th>{t("tables.nullable")}</th>
                <th>PK</th>
              </tr>
            </thead>
            <tbody>
              {table.columns.map((c) => (
                <tr key={c.name}>
                  <td>{c.name}</td>
                  <td className="muted">{c.dataType}</td>
                  <td>{c.nullable ? t("common.yes") : t("common.no")}</td>
                  <td>{c.pk ? "PK" : ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="struct-meta">
            <div>
              <strong>{t("tables.indexes")}</strong>
              {table.indexes.length ? (
                <ul>
                  {table.indexes.map((ix) => (
                    <li key={ix.name}>
                      {ix.name}
                      {ix.unique ? ` (${t("tables.unique")})` : ""}: {ix.columns.join(", ")}
                    </li>
                  ))}
                </ul>
              ) : (
                <span className="muted"> {t("common.none")}</span>
              )}
            </div>
            <div>
              <strong>{t("tables.foreignKeys")}</strong>
              {table.foreignKeys.length ? (
                <ul>
                  {table.foreignKeys.map((fk) => (
                    <li key={`${fk.column}-${fk.referencesTable}-${fk.referencesColumn}`}>
                      {fk.column} → {fk.referencesSchema ? `${fk.referencesSchema}.` : ""}
                      {fk.referencesTable}.{fk.referencesColumn}
                    </li>
                  ))}
                </ul>
              ) : (
                <span className="muted"> {t("common.none")}</span>
              )}
            </div>
          </div>
        </div>
      )}

      <div className="grid-toolbar">
        <button
          className="btn small"
          disabled={!canEdit}
          title={canEdit ? undefined : noEditTitle}
          onClick={() => openEdit("insert")}
        >
          {t("tables.insert")}
        </button>
        <button
          className="btn small"
          disabled={!canEdit || selected == null}
          title={canEdit ? undefined : noEditTitle}
          onClick={() => openEdit("edit")}
        >
          {t("tables.edit")}
        </button>
        <button
          className="btn small"
          disabled={!canEdit || selected == null}
          title={canEdit ? undefined : noEditTitle}
          onClick={doDelete}
        >
          {t("tables.delete")}
        </button>
        <details className="toolbar-menu">
          <summary className="btn small">{t("tables.more")}</summary>
          <div className="toolbar-menu-panel">
            <button
              className="btn small"
              disabled={!canEdit || selected == null}
              title={canEdit ? undefined : noEditTitle}
              onClick={() => openEdit("duplicate")}
            >
              {t("tables.duplicate")}
            </button>
            <button className="btn small" disabled={selected == null} onClick={() => copyRow(false)}>
              {t("tables.copyTsv")}
            </button>
            <button className="btn small" disabled={selected == null} onClick={() => copyRow(true)}>
              {t("tables.copyJson")}
            </button>
            <button
              className="btn small"
              disabled={!rows}
              title={t("tables.exportPageTitle")}
              onClick={() =>
                result && downloadCsv(`${table.name}-page${page + 1}-${stamp()}`, result.columns, result.rows)
              }
            >
              {t("tables.exportCsv")}
            </button>
            <button
              className="btn small"
              disabled={!rows}
              title={t("tables.exportPageTitle")}
              onClick={() =>
                result && downloadJson(`${table.name}-page${page + 1}-${stamp()}`, result.columns, result.rows)
              }
            >
              {t("tables.exportJson")}
            </button>
          </div>
        </details>
        {activeFilters > 0 && (
          <>
            <span className="tb-sep" />
            <span className="muted">
              {t(activeFilters > 1 ? "tables.activeFiltersPlural" : "tables.activeFilters", {
                count: activeFilters,
              })}
            </span>
            <button className="btn small" onClick={() => setFilters({})}>
              {t("tables.clear")}
            </button>
          </>
        )}
      </div>

      {err && <div className="error">{err}</div>}

      {/* Dim (not blank) the stale grid while paging/sorting/filtering re-queries. */}
      <div className={busy && result ? "table-data-body busy" : "table-data-body"}>
        {result ? (
          result.rows.length ? (
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
          ) : busy ? (
            // Reloading (filter cleared / table switched) — the stale zero-row result would
            // otherwise flash a wrong "Table is empty." against the now-live filter state.
            <div className="muted loading">{t("tables.loadingRows")}</div>
          ) : (
            // Loaded but zero rows: distinguish an empty table from a filter that matched nothing.
            <div className="muted">
              {activeFilters > 0 ? t("tables.noRowsFilter") : t("tables.tableEmpty")}
            </div>
          )
        ) : (
          !err && <div className={busy ? "muted loading" : "muted"}>{busy ? t("tables.loadingRows") : t("tables.noRows")}</div>
        )}

        {panelOpen && (
          <aside className="grid-panel">
            {editor && !prepared && (
              <RowEditor
                key={`${editor.mode}-${selected}`}
                engine={engine}
                table={table}
                mode={editor.mode}
                initial={editor.initial}
                onSubmit={(write: RowEditorSubmission) => setPrepared(write)}
                onCancel={() => {
                  setEditor(null);
                  setPrepared(null);
                }}
              />
            )}
            {pendingDelete && (
              <div className="row-editor">
                <div className="panel-head">
                  <strong>{t("tables.deleteRow")}</strong>
                  <button className="btn small" aria-label={t("common.cancel")} onClick={() => setPendingDelete(null)}>
                    <Icon name="close" />
                  </button>
                </div>
                <div className="row-fields">
                  {Object.entries(pendingDelete).map(([k, v]) => (
                    <div className="row-field" key={k}>
                      <label>
                        {k}
                        <span className="pk-badge">PK</span>
                      </label>
                      <code>{v == null ? "NULL" : v}</code>
                    </div>
                  ))}
                </div>
                <div className="row-editor-actions">
                  <button className="btn primary" onClick={armDelete}>
                    {t("tables.reviewDelete")}
                  </button>
                  <button className="btn" onClick={() => setPendingDelete(null)}>
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            )}
            {prepared && (
              <ApprovalCard
                key={prepared.sql}
                connectionId={connection.id}
                engine={engine}
                sql={prepared.sql}
                safety={safety}
                rationale={prepared.rationale}
                collapseSql={prepared.collapseSql}
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
