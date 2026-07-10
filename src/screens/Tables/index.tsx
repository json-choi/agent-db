// Table data view (DataGrip-style data editor). Server-side pagination with a STABLE
// ORDER BY (primary key) + a real COUNT(*) so pages never repeat/skip rows and the
// total is exact ("rows X-Y of Z"). Column sort and per-column filters go through the
// same sqlBuild helpers. Row edits (insert/update/delete) are generated as SQL and
// routed through ApprovalCard, so the full safety pipeline (classify/preview/approve/
// audit) applies — reads still auto-run and never need approval.
import { useEffect, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import type {
  CatalogTable,
  ConnectionProfile,
  ExecOutcome,
  SafetySettings,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import DataGrid from "../../components/DataGrid";
import { Icon } from "../../components/Icon";
import CellViewer from "../../components/CellViewer";
import RowEditor, { type RowEditorSubmission } from "../../components/RowEditor";
import ApprovalCard from "../../components/ApprovalCard";
import Skeleton from "../../components/Skeleton";
import { useToast } from "../../components/Toast";
import { tableRowsQuery } from "../../lib/queries";
import { tableKey, tableLabel } from "../../lib/tableRef";
import { downloadCsv, downloadJson, stamp } from "../../lib/export";
import { useI18n } from "../../lib/i18n";
import {
  buildDelete,
  cellToInput,
  hasNonScalarPk,
  pkColumns,
  type GridSort,
} from "../../lib/sqlBuild";
import "./tables.css";

const FILTER_DEBOUNCE_MS = 250;

function sameFilters(a: Record<string, string>, b: Record<string, string>) {
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((k) => a[k] === b[k]);
}

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
  const [writeErr, setWriteErr] = useState<string | null>(null);
  const [page, setPage] = useState(0);
  const [sort, setSort] = useState<GridSort | null>(null);
  const [filters, setFilters] = useState<Record<string, string>>({});
  // Typing in a filter must not fire a query per keystroke, so the query reads the settled
  // value while the inputs stay controlled by `filters`.
  const [appliedFilters, setAppliedFilters] = useState<Record<string, string>>({});
  const [selected, setSelected] = useState<number | null>(null);
  const [cellSel, setCellSel] = useState<CellSel | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const [prepared, setPrepared] = useState<PreparedWrite | null>(null);
  // Readable confirm gate for DELETE: PK pairs of the target row, mirroring how
  // insert/edit/duplicate pass through RowEditor before arming the ApprovalCard.
  const [pendingDelete, setPendingDelete] = useState<Record<string, string | null> | null>(null);
  const [structure, setStructure] = useState(false);

  const pageSize = Math.min(PAGE, safety.maxRows || PAGE);
  const key = tableKey(table);

  // Reset the whole view when the selected table changes. Done during render (not in an
  // effect) so the row query never fires once with the new table and the old table's
  // filters, sort, or page.
  const [viewKey, setViewKey] = useState(key);
  if (viewKey !== key) {
    setViewKey(key);
    setPage(0);
    setSort(null);
    setFilters({});
    setAppliedFilters({});
    setSelected(null);
    setCellSel(null);
    setEditor(null);
    setPrepared(null);
    setPendingDelete(null);
    setStructure(false);
    setWriteErr(null);
  }

  const rowsQuery = useQuery({
    ...tableRowsQuery({
      connectionId: connection.id,
      engine,
      table,
      filters: appliedFilters,
      sort,
      pageSize,
      page,
    }),
    // Paging and filtering repaint the previous page (dimmed) instead of blanking the grid.
    placeholderData: keepPreviousData,
  });

  const result = rowsQuery.data?.result ?? null;
  const total = rowsQuery.data?.total ?? null;
  const busy = rowsQuery.isFetching;
  const err = writeErr ?? (rowsQuery.error ? errMessage(rowsQuery.error) : null);

  // Settling a filter always returns to the first page; both land in one render so only the
  // final query key is ever fetched. The equality guard keeps this inert on mount and on a
  // table switch, where a stray timer would otherwise yank the user back to page 0.
  useEffect(() => {
    if (sameFilters(filters, appliedFilters)) return;
    const timer = window.setTimeout(() => {
      setAppliedFilters(filters);
      setPage(0);
    }, FILTER_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [filters, appliedFilters]);
  // Row editing needs a PK we can match on. No PK, or a PK whose rendered cell value can't
  // round-trip to a literal (binary/json/array/composite), both disable it — same as noPk.
  const nonScalarPk = hasNonScalarPk(table);
  const canEdit = pkColumns(table).length > 0 && !nonScalarPk;
  const activeFilters = Object.values(filters).filter((v) => v.trim()).length;

  // Fresh rows landed, so any row/cell the user had selected now points at data that may
  // no longer be there, and a stale write error no longer describes what is on screen.
  useEffect(() => {
    setSelected(null);
    setCellSel(null);
    setWriteErr(null);
  }, [rowsQuery.dataUpdatedAt]);

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
    setPage(0);
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
        rationale: t("rowEditor.rationaleDelete", { table: table.name }),
        collapseSql: true,
      });
      setPendingDelete(null);
    } catch (e) {
      setWriteErr(errMessage(e));
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
    setWriteErr(null);
    void rowsQuery.refetch();
  }

  const noEditTitle = nonScalarPk
    ? t("tables.nonScalarPk")
    : t("tables.noTablePk");
  const panelOpen = !!prepared || !!editor || !!cellSel || !!pendingDelete;

  return (
    <div className="table-data">
      <div className="table-data-head ds-workbench-head">
        <div className="ds-workbench-title">
          <h3>{t("tables.editor")}</h3>
          <div className="ds-title-line">
            <strong>{tableLabel(engine, table)}</strong>
            <span className="ds-context-badge">{table.kind === "view" ? t("schema.view") : t("tables.sourceTable")}</span>
          </div>
          <div className="ds-meta-row">
            <span>{t("tables.cols", { count: table.columns.length })}</span>
            <span className="ds-meta-dot" />
            <span>LIMIT {pageSize.toLocaleString()}</span>
            {result && (
              <>
                <span className="ds-meta-dot" />
                <span>
                  {total != null
                    ? t("tables.rowRangeTotal", {
                        from,
                        to,
                        total: total.toLocaleString(),
                      })
                    : t("tables.rowRange", { from, to })}
                  {result.truncated ? " (truncated)" : ""}
                </span>
                <span className="ds-meta-dot" />
                <span>{result.durationMs} ms</span>
              </>
            )}
          </div>
        </div>
        <div className="table-pager ds-command-group" aria-label={t("tables.pagination")}>
          <button className="btn small" disabled={busy || !hasPrev} onClick={() => setPage(0)}>
            « {t("common.first")}
          </button>
          <button
            className="btn small"
            disabled={busy || !hasPrev}
            onClick={() => setPage(page - 1)}
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
            onClick={() => setPage(page + 1)}
          >
            {t("common.next")} ›
          </button>
          <button
            className="btn small"
            disabled={busy || lastPage == null || !hasNext}
            onClick={() => lastPage != null && setPage(lastPage)}
          >
            {t("tables.last")} »
          </button>
          <button
            className="btn small refresh"
            disabled={busy}
            aria-label={t("common.refresh")}
            title={t("common.refresh")}
            onClick={() => void rowsQuery.refetch()}
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

      <div className="table-query-strip ds-filter-strip" aria-label={t("tables.querySurface")}>
        <span className={activeFilters ? "ds-filter-token active" : "ds-filter-token"}>
          <strong>WHERE</strong>
          {activeFilters
            ? t(activeFilters > 1 ? "tables.activeFiltersPlural" : "tables.activeFilters", {
                count: activeFilters,
              })
            : t("tables.noFilters")}
        </span>
        <span className={sort ? "ds-filter-token active" : "ds-filter-token"}>
          <strong>ORDER BY</strong>
          {sort ? `${sort.col} ${sort.dir.toUpperCase()}` : t("tables.unsorted")}
        </span>
        <span className={safety.allowWrites ? "ds-filter-token risk" : "ds-filter-token"}>
          <strong>{t("tables.writePolicy")}</strong>
          {safety.allowWrites ? t("tables.writePolicyWrites") : t("tables.writePolicyReadonly")}
        </span>
        <span className="ds-toolbar-spacer" />
        <span className="ds-filter-token column-policy" title={t("tables.columnPolicyHint")}>
          <strong>{t("tables.columnPolicy")}</strong>
          {t("tables.columnPolicyCompact")}
        </span>
        {activeFilters > 0 && (
          <button className="btn small" onClick={() => setFilters({})}>
            {t("tables.clear")}
          </button>
        )}
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

      <div className="grid-toolbar ds-data-toolbar">
        <div className="ds-toolbar-group">
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
            className="btn small danger"
            disabled={!canEdit || selected == null}
            title={canEdit ? undefined : noEditTitle}
            onClick={doDelete}
          >
            {t("tables.delete")}
          </button>
        </div>
        <span className="ds-toolbar-spacer" />
        <div className="ds-toolbar-group">
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
        </div>
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
          // No cached page for this table yet — the only place a cold load is visible.
          !err && (busy ? <Skeleton lines={8} /> : <div className="muted">{t("tables.noRows")}</div>)
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
                <div className="row-editor-actions ds-action-row">
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
