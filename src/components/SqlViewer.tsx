// CodeMirror 6 SQL viewer/editor, shared by the Ask screen, ApprovalCard, and the SQL
// screen. Read-only by default; when a `catalog` is passed it feeds schema-aware
// autocomplete (table + column names), and `onRun` binds Mod-Enter to execute.
import { useEffect, useMemo, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { sql, type SQLNamespace } from "@codemirror/lang-sql";
import { EditorView, keymap } from "@codemirror/view";
import type { Catalog } from "../ipc/types";

// Catalog → CodeMirror schema map. Each table contributes its columns under the bare
// name, and (for Postgres, where `schema` is set) also under a schema namespace so
// `public.users.` completes. ponytail: bare-name collisions across schemas take the
// last table — fine for autocomplete hints.
function buildSchema(catalog: Catalog): SQLNamespace {
  const ns: Record<string, SQLNamespace> = {};
  for (const t of catalog.tables) {
    const cols = t.columns.map((c) => c.name);
    ns[t.name] = cols;
    if (t.schema) {
      const s = (ns[t.schema] ??= {}) as Record<string, SQLNamespace>;
      s[t.name] = cols;
    }
  }
  return ns;
}

export interface SqlViewerProps {
  value: string;
  editable?: boolean;
  onChange?: (v: string) => void;
  onRun?: (selectedSql?: string) => void;
  catalog?: Catalog;
  minHeight?: string;
}

export default function SqlViewer({
  value,
  editable = false,
  onChange,
  onRun,
  catalog,
  minHeight = "80px",
}: SqlViewerProps) {
  // Match CodeMirror's built-in theme to the OS scheme so it isn't a bright white
  // slab inside dark panels. No new dependency — 'light'/'dark' ship with the lib.
  const [dark, setDark] = useState(
    () => window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false,
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (e: MediaQueryListEvent) => setDark(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  const extensions = useMemo(() => {
    const ext = [
      sql(catalog ? { schema: buildSchema(catalog) } : undefined),
      EditorView.lineWrapping,
    ];
    if (onRun) {
      ext.push(
        keymap.of([
          {
            key: "Mod-Enter",
            run: (view) => {
              // Run just the selection when there is one; otherwise the whole draft.
              const sel = view.state.selection.main;
              const picked = sel.empty ? undefined : view.state.sliceDoc(sel.from, sel.to);
              onRun(picked);
              return true;
            },
          },
        ]),
      );
    }
    return ext;
  }, [catalog, onRun]);

  return (
    <CodeMirror
      value={value}
      theme={dark ? "dark" : "light"}
      editable={editable}
      readOnly={!editable}
      onChange={onChange}
      extensions={extensions}
      basicSetup={{ lineNumbers: true, foldGutter: false }}
      style={{ minHeight, fontSize: "13px" }}
    />
  );
}
