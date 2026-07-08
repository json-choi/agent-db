// Row editor panel: one input per column (checkbox for SQL NULL, numeric input for
// numeric columns, PK fields readonly on edit). Generates an INSERT/UPDATE via sqlBuild
// and hands the SQL up via onSubmit — the parent routes it through ApprovalCard so the
// safety pipeline (classify/preview/approve/audit) applies. Never executes directly.
import { useMemo, useState } from "react";
import { Icon } from "./Icon";
import type { CatalogTable, Engine } from "../ipc/types";
import { buildInsert, buildUpdate, isNumericType, pkColumns } from "../lib/sqlBuild";
import "./grid.css";

type Mode = "insert" | "edit" | "duplicate";

export type RowEditorSubmission = {
  sql: string;
  rationale: string;
  collapseSql: boolean;
};

function shortValue(value: string | null): string {
  if (value === null) return "NULL";
  if (value === "") return "(empty)";
  return value.length > 28 ? `${value.slice(0, 25)}...` : value;
}

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

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
  onSubmit: (write: RowEditorSubmission) => void;
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
  const changedColumns = useMemo(
    () =>
      isEdit
        ? table.columns.filter((c) => !c.pk && vals[c.name] !== initial[c.name])
        : table.columns.filter((c) => c.name in vals),
    [initial, isEdit, table.columns, vals],
  );
  const changedCount = changedColumns.length;

  function rationaleForEdit(): string {
    if (changedColumns.length === 1) {
      const c = changedColumns[0];
      return `Change ${c.name}: ${shortValue(initial[c.name] ?? null)} -> ${shortValue(vals[c.name] ?? null)}.`;
    }
    const names = changedColumns.slice(0, 4).map((c) => c.name);
    const rest = changedColumns.length > names.length ? `, +${changedColumns.length - names.length} more` : "";
    return `Change ${plural(changedColumns.length, "field")}: ${names.join(", ")}${rest}.`;
  }

  function submit() {
    try {
      let sql: string;
      let rationale: string;
      if (isEdit) {
        if (!changedColumns.length) {
          setError("No changes to save.");
          return;
        }
        const pkValues: Record<string, string | null> = {};
        for (const c of pkColumns(table)) pkValues[c.name] = initial[c.name] ?? null;
        const setValues: Record<string, string | null> = {};
        for (const c of changedColumns) setValues[c.name] = vals[c.name] ?? null;
        sql = buildUpdate(engine, table, pkValues, setValues);
        rationale = rationaleForEdit();
      } else {
        sql = buildInsert(engine, table, vals);
        rationale =
          mode === "duplicate"
            ? `Insert a duplicated row into ${table.name} with ${plural(changedColumns.length, "column")}.`
            : `Insert a new row into ${table.name} with ${plural(changedColumns.length, "column")}.`;
      }
      setError(null);
      onSubmit({ sql, rationale, collapseSql: true });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  const title = isEdit ? "Edit row" : mode === "duplicate" ? "Duplicate row" : "Insert row";
  const actionText = isEdit
    ? changedCount
      ? `Review ${plural(changedCount, "change")}`
      : "No changes"
    : "Review row";
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
        <div className="row-editor-title">
          <strong>{title}</strong>
          {isEdit && <span className="muted">{plural(changedCount, "change")}</span>}
        </div>
        <button className="btn small" onClick={onCancel} aria-label="Close">
          <Icon name="close" />
        </button>
      </div>
      <div className="row-fields">
        {table.columns.filter((c) => !(isEdit && c.pk)).map((c) => {
          const isNull = vals[c.name] === null;
          const readonly = isEdit && c.pk;
          const changed = isEdit && !c.pk && vals[c.name] !== initial[c.name];
          return (
            <div className={changed ? "row-field changed" : "row-field"} key={c.name}>
              <label title={c.dataType}>
                {c.name}
                {c.pk && <span className="pk-badge">PK</span>}
                {changed && <span className="changed-badge">Changed</span>}
              </label>
              <div className="row-field-input">
                <input
                  type={isNumericType(c.dataType) ? "number" : "text"}
                  value={isNull ? "" : vals[c.name] ?? ""}
                  disabled={isNull || readonly}
                  onChange={(e) => set(c.name, e.target.value)}
                />
                <label
                  className={
                    "null-toggle" +
                    (isNull ? " active" : "") +
                    (readonly || !c.nullable ? " disabled" : "")
                  }
                  title={c.nullable ? "SQL NULL" : "column is NOT NULL"}
                >
                  <input
                    type="checkbox"
                    aria-label={`Set ${c.name} to SQL NULL`}
                    checked={isNull}
                    disabled={readonly || !c.nullable}
                    onChange={(e) => set(c.name, e.target.checked ? null : "")}
                  />
                  <Icon name="circleSlash" />
                </label>
              </div>
            </div>
          );
        })}
      </div>
      {error && <div className="error">{error}</div>}
      <div className="row-editor-actions">
        <button className="btn primary" disabled={isEdit && changedCount === 0} onClick={submit}>
          {actionText}
        </button>
        <button className="btn" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}
