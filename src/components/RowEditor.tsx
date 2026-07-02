// Row editor panel: one input per column (checkbox for SQL NULL, numeric input for
// numeric columns, PK fields readonly on edit). Generates an INSERT/UPDATE via sqlBuild
// and hands the SQL up via onSubmit — the parent routes it through ApprovalCard so the
// safety pipeline (classify/preview/approve/audit) applies. Never executes directly.
import { useState } from "react";
import type { CatalogTable, Engine } from "../ipc/types";
import { buildInsert, buildUpdate, isNumericType, pkColumns } from "../lib/sqlBuild";
import "./grid.css";

type Mode = "insert" | "edit" | "duplicate";

export default function RowEditor({
  engine,
  table,
  mode,
  initial,
  onSubmit,
  onCancel,
}: {
  engine: Engine;
  table: CatalogTable;
  mode: Mode;
  initial: Record<string, string | null>; // {} for a fresh insert
  onSubmit: (sql: string) => void;
  onCancel: () => void;
}) {
  // Only seed columns present in `initial` (edit/duplicate). A fresh insert (initial={})
  // starts empty so untouched columns are OMITTED from the INSERT — letting serial PKs and
  // column defaults fire instead of forcing an invalid '' into every field.
  const [vals, setVals] = useState<Record<string, string | null>>(() => {
    const v: Record<string, string | null> = {};
    for (const c of table.columns) if (c.name in initial) v[c.name] = initial[c.name];
    return v;
  });
  const [error, setError] = useState<string | null>(null);

  const isEdit = mode === "edit";
  const set = (name: string, value: string | null) => setVals((p) => ({ ...p, [name]: value }));

  function submit() {
    try {
      let sql: string;
      if (isEdit) {
        const pkValues: Record<string, string | null> = {};
        for (const c of pkColumns(table)) pkValues[c.name] = initial[c.name] ?? null;
        const setValues: Record<string, string | null> = {};
        for (const c of table.columns) if (!c.pk) setValues[c.name] = vals[c.name];
        sql = buildUpdate(engine, table, pkValues, setValues);
      } else {
        sql = buildInsert(engine, table, vals);
      }
      setError(null);
      onSubmit(sql);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  const title = isEdit ? "Edit row" : mode === "duplicate" ? "Duplicate row" : "Insert row";
  return (
    <div
      className="row-editor"
      onKeyDown={(e) => {
        if (e.key === "Enter" && (e.target as HTMLElement).tagName === "INPUT") {
          e.preventDefault();
          submit();
        } else if (e.key === "Escape") {
          onCancel();
        }
      }}
    >
      <div className="panel-head">
        <strong>{title}</strong>
        <button className="btn small" onClick={onCancel}>
          ✕
        </button>
      </div>
      <div className="row-fields">
        {table.columns.map((c) => {
          const isNull = vals[c.name] === null;
          const readonly = isEdit && c.pk;
          return (
            <div className="row-field" key={c.name}>
              <label>
                {c.name}
                {c.pk && <span className="pk-badge">PK</span>}
                <span className="muted type">{c.dataType}</span>
              </label>
              <div className="row-field-input">
                <input
                  type={isNumericType(c.dataType) ? "number" : "text"}
                  value={isNull ? "" : vals[c.name] ?? ""}
                  disabled={isNull || readonly}
                  onChange={(e) => set(c.name, e.target.value)}
                />
                <label
                  className="null-toggle"
                  title={c.nullable ? "SQL NULL" : "column is NOT NULL"}
                >
                  <input
                    type="checkbox"
                    checked={isNull}
                    disabled={readonly || !c.nullable}
                    onChange={(e) => set(c.name, e.target.checked ? null : "")}
                  />
                  NULL
                </label>
              </div>
            </div>
          );
        })}
      </div>
      {error && <div className="error">{error}</div>}
      <div className="row-editor-actions">
        <button className="btn primary" onClick={submit}>
          Review SQL
        </button>
        <button className="btn" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}
