// Shared results table. Renders whatever rows it's handed (callers window/cap first).
// Sticky header + row numbers + null styling. All interactivity is opt-in via callbacks
// so the plain read-only callers (Sql console, MCP result) render unchanged:
//   - onSort     → clickable headers that cycle asc/desc/none (arrow on the sorted col)
//   - onFilter   → a per-column filter row under the header
//   - onSelectRow/onCellClick → row highlight + click-to-open a cell in the side viewer
//   - startIndex → row numbers continue across pages (rows 101-200, not 1-100 again)
// Columns are drag-resizable: first drag snapshots every column's rendered width and
// flips the table to fixed layout so only the dragged column moves. Double-click a
// handle to reset all widths (back to auto layout). Widths reset per column set.
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { QueryResult } from "../ipc/types";
import type { GridSort } from "../lib/sqlBuild";
import { Icon } from "./Icon";
import { useI18n } from "../lib/i18n";
import "./grid.css";

function cell(v: unknown): string {
  if (v === null || v === undefined) return "NULL";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

// Clipboard text for a selected cell — same rules as CellViewer's Copy:
// null/undefined → "NULL", objects → pretty JSON, JSON-string → pretty JSON, else String.
function copyText(v: unknown): string {
  if (v === null || v === undefined) return "NULL";
  if (typeof v === "object") return JSON.stringify(v, null, 2);
  const s = String(v);
  if (typeof v === "string") {
    try {
      const p = JSON.parse(s);
      if (p && typeof p === "object") return JSON.stringify(p, null, 2);
    } catch {
      /* not JSON — plain text */
    }
  }
  return s;
}

export default function DataGrid({
  result,
  startIndex = 0,
  sort,
  onSort,
  filters,
  onFilter,
  selectedRow,
  onSelectRow,
  onCellClick,
}: {
  result: QueryResult;
  startIndex?: number;
  sort?: GridSort | null;
  onSort?: (col: string) => void;
  filters?: Record<string, string>;
  onFilter?: (col: string, value: string) => void;
  selectedRow?: number | null;
  onSelectRow?: (i: number) => void;
  onCellClick?: (value: unknown, rowIndex: number, col: string) => void;
}) {
  const { t } = useI18n();
  const interactive = !!onSelectRow || !!onCellClick;
  // Column widths keyed by header-cell index (0 = rownum). Empty map = auto layout.
  const [widths, setWidths] = useState<Record<number, number>>({});
  // Selected cell (click to select, ⌘C to copy, Esc to clear). Independent of onCellClick.
  const [sel, setSel] = useState<{ row: number; col: number } | null>(null);
  const tableRef = useRef<HTMLTableElement>(null);
  const sig = result.columns.join(" ");
  useEffect(() => {
    setWidths({}); // new column set → stale widths dropped
  }, [sig]);
  useEffect(() => {
    // Sort/filter/pagination swap the rows without changing columns — a selection is
    // coordinates into rows, so any new result object invalidates it.
    setSel(null);
  }, [result]);
  const fixed = Object.keys(widths).length > 0;
  const totalW = fixed
    ? Object.values(widths).reduce((a, b) => a + b, 0)
    : undefined;

  // Right-align numeric columns. NUMERIC/MONEY arrive as plain decimal strings (the
  // Rust side serializes them lossless), so detect by value shape — and per column,
  // not per cell, so a text column with the odd digit-only value can't render ragged.
  const numericCols = useMemo(() => {
    const numRe = /^-?\d+(\.\d+)?$/;
    return result.columns.map(
      (_, j) =>
        result.rows.some((r) => r[j] != null) &&
        result.rows.every((r) => {
          const v = r[j];
          return (
            v == null ||
            typeof v === "number" ||
            (typeof v === "string" && numRe.test(v))
          );
        }),
    );
  }, [result]);

  function startResize(
    e: { preventDefault(): void; stopPropagation(): void; clientX: number },
    colIdx: number,
  ) {
    e.preventDefault();
    e.stopPropagation(); // don't trigger the sort click on the header
    const table = tableRef.current;
    if (!table) return;
    const ths = Array.from(
      table.querySelectorAll<HTMLTableCellElement>("thead tr:first-child th"),
    );
    const snap: Record<number, number> = { ...widths };
    ths.forEach((th, i) => {
      if (snap[i] == null) snap[i] = th.offsetWidth;
    });
    const startX = e.clientX;
    const startW = snap[colIdx];
    // Coalesce mousemoves to one setWidths per frame — reconciling the whole tbody
    // on every pointer event churns badly on large result sets.
    let raf = 0;
    let pendingW = startW;
    const flush = () => {
      raf = 0;
      setWidths({ ...snap, [colIdx]: pendingW });
    };
    const move = (ev: MouseEvent) => {
      pendingW = Math.min(1200, Math.max(50, startW + ev.clientX - startX));
      if (!raf) raf = requestAnimationFrame(flush);
    };
    const up = () => {
      if (raf) cancelAnimationFrame(raf);
      flush(); // commit the final position (a queued frame may not have run)
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  }

  // Click and Enter (see below) both land here so keyboard nav opens the same viewer a click does.
  function selectCell(i: number, j: number) {
    setSel({ row: i, col: j });
    onSelectRow?.(i);
    onCellClick?.(result.rows[i]?.[j], i, result.columns[j]);
  }

  // ⌘C/Ctrl+C copies the selected cell — but yield to a real text selection so users
  // can still copy dragged-over text normally. Esc clears the cell selection. Arrow keys
  // roving-select a cell (mirrors App.tsx's tab-bar pattern: move sel, then focus the td —
  // valid even at tabIndex=-1, only Tab-order membership depends on that). Enter opens it.
  function onKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Escape" && sel) {
      setSel(null);
      return;
    }
    if ((e.metaKey || e.ctrlKey) && (e.key === "c" || e.key === "C") && sel) {
      if ((window.getSelection()?.toString() ?? "") !== "") return; // real selection wins
      e.preventDefault();
      void navigator.clipboard.writeText(copyText(result.rows[sel.row]?.[sel.col]));
      return;
    }
    if (
      (e.key === "ArrowUp" || e.key === "ArrowDown" || e.key === "ArrowLeft" || e.key === "ArrowRight") &&
      result.rows.length > 0
    ) {
      e.preventDefault();
      const cur = sel ?? { row: 0, col: 0 };
      const row =
        e.key === "ArrowUp"
          ? Math.max(0, cur.row - 1)
          : e.key === "ArrowDown"
            ? Math.min(result.rows.length - 1, cur.row + 1)
            : cur.row;
      const col =
        e.key === "ArrowLeft"
          ? Math.max(0, cur.col - 1)
          : e.key === "ArrowRight"
            ? Math.min(result.columns.length - 1, cur.col + 1)
            : cur.col;
      setSel({ row, col });
      tableRef.current
        ?.querySelector<HTMLTableCellElement>(
          `tbody tr:nth-child(${row + 1}) td:nth-child(${col + 2})`,
        )
        ?.focus();
      return;
    }
    if (e.key === "Enter" && sel) {
      selectCell(sel.row, sel.col);
    }
  }

  return (
    <div className="grid-scroll" tabIndex={0} onKeyDown={onKeyDown}>
      <table
        ref={tableRef}
        className={fixed ? "grid fixed" : "grid"}
        style={fixed ? { tableLayout: "fixed", width: totalW } : undefined}
      >
        {fixed && (
          <colgroup>
            <col style={{ width: widths[0] }} />
            {result.columns.map((_, j) => (
              <col key={j} style={{ width: widths[j + 1] }} />
            ))}
          </colgroup>
        )}
        <thead>
          <tr>
            <th className="rownum">#</th>
            {result.columns.map((c, j) => (
              <th
                key={c}
                className={onSort ? "sortable" : undefined}
                role={onSort ? "button" : undefined}
                tabIndex={onSort ? 0 : undefined}
                aria-sort={
                  onSort
                    ? sort?.col === c
                      ? sort.dir === "asc"
                        ? "ascending"
                        : "descending"
                      : "none"
                    : undefined
                }
                onClick={onSort ? () => onSort(c) : undefined}
                onKeyDown={
                  onSort
                    ? (e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          onSort(c);
                        }
                      }
                    : undefined
                }
              >
                {c}
                {sort?.col === c && (
                  <span className="sort-arrow">
                    {" "}
                    <Icon name={sort.dir === "asc" ? "caretUp" : "caretDown"} />
                  </span>
                )}
                <span
                  className="col-resizer"
                  title={t("grid.resizeHint")}
                  onMouseDown={(e) => startResize(e, j + 1)}
                  onClick={(e) => e.stopPropagation()}
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    setWidths({});
                  }}
                />
              </th>
            ))}
          </tr>
          {onFilter && (
            <tr className="filter-row">
              <th className="rownum" />
              {result.columns.map((c) => (
                <th key={c}>
                  <input
                    className="filter-input"
                    value={filters?.[c] ?? ""}
                    placeholder={t("grid.filterPlaceholder")}
                    aria-label={t("grid.filterLabel", { col: c })}
                    onChange={(e) => onFilter(c, e.target.value)}
                  />
                </th>
              ))}
            </tr>
          )}
        </thead>
        <tbody>
          {result.rows.map((row, i) => (
            <tr
              key={i}
              className={selectedRow === i ? "selected" : undefined}
            >
              <td
                className="rownum"
                role={onSelectRow ? "button" : undefined}
                tabIndex={onSelectRow ? 0 : undefined}
                onClick={onSelectRow ? () => onSelectRow(i) : undefined}
                onKeyDown={
                  onSelectRow
                    ? (e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          onSelectRow(i);
                        }
                      }
                    : undefined
                }
              >
                {startIndex + i + 1}
              </td>
              {row.map((v, j) => {
                const text = cell(v);
                const isSel = sel?.row === i && sel.col === j;
                return (
                  <td
                    key={j}
                    className={
                      (v === null ? "nullcell" : "") +
                      (numericCols[j] ? " numcell" : "") +
                      (interactive ? " clickable" : "") +
                      (isSel ? " cell-sel" : "")
                    }
                    // fixed layout clips by width not length, so any value can be truncated → always title it
                    title={
                      fixed || text.length > 40 || text.includes("\n") ? text : undefined
                    }
                    // Roving tabindex: only the selected cell is a tab stop; arrows move it (onKeyDown above).
                    tabIndex={isSel ? 0 : -1}
                    aria-selected={isSel}
                    onClick={() => selectCell(i, j)}
                  >
                    {text}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
