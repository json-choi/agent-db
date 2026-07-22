// Row editor panel: one input per column (checkbox for SQL NULL, numeric input for
// numeric columns, PK fields readonly on edit). Generates an INSERT/UPDATE via sqlBuild
// and hands the SQL up via onSubmit — the parent routes it through ApprovalCard so the
// safety pipeline (classify/preview/approve/audit) applies. Never executes directly.
import { useMemo, useState } from "react";
import { useI18n } from "../lib/i18n";
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
  const { t } = useI18n();
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
      return t("rowEditor.rationaleFieldChange", {
        col: c.name,
        from: shortValue(initial[c.name] ?? null),
        to: shortValue(vals[c.name] ?? null),
      });
    }
    const names = changedColumns.slice(0, 4).map((c) => c.name);
    const rest =
      changedColumns.length > names.length
        ? t("rowEditor.rationaleMore", { count: changedColumns.length - names.length })
        : "";
    return t("rowEditor.rationaleFieldsChange", { count: changedColumns.length, names: names.join(", "), rest });
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
        rationale = t(
          mode === "duplicate"
            ? changedColumns.length === 1
              ? "rowEditor.rationaleDuplicate"
              : "rowEditor.rationaleDuplicatePlural"
            : changedColumns.length === 1
              ? "rowEditor.rationaleInsert"
              : "rowEditor.rationaleInsertPlural",
          { table: table.name, count: changedColumns.length },
        );
      }
      setError(null);
      onSubmit({ sql, rationale, collapseSql: true });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  const title = t(
    isEdit ? "rowEditor.titleEdit" : mode === "duplicate" ? "rowEditor.titleDuplicate" : "rowEditor.titleInsert",
  );
  const actionText = isEdit
    ? changedCount
      ? t(changedCount === 1 ? "rowEditor.reviewChange" : "rowEditor.reviewChangePlural", { count: changedCount })
      : t("rowEditor.noChanges")
    : t("rowEditor.reviewRow");
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
          {isEdit && (
            <span className="muted">
              {t(changedCount === 1 ? "rowEditor.changeCount" : "rowEditor.changeCountPlural", {
                count: changedCount,
              })}
            </span>
          )}
        </div>
        <button className="btn small" onClick={onCancel} aria-label={t("common.close")}>
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
                {c.pk && <span className="pk-badge">{t("schema.pk")}</span>}
                {changed && <span className="changed-badge">{t("rowEditor.changedBadge")}</span>}
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
                  title={c.nullable ? "SQL NULL" : t("rowEditor.notNullTitle")}
                >
                  <input
                    type="checkbox"
                    aria-label={t("rowEditor.setNullAria", { col: c.name })}
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
      <div className="row-editor-actions ds-action-row ds-control-row">
        <button className="btn primary" disabled={isEdit && changedCount === 0} onClick={submit}>
          {actionText}
        </button>
        <button className="btn" onClick={onCancel}>
          {t("common.cancel")}
        </button>
      </div>
    </div>
  );
}
