// Connection sidebar list + create/edit form. Secrets are never held here after
// save — the password is handed to the backend, which stores it in the Keychain.
import { useEffect, useRef, useState } from "react";
import {
  deleteConnection,
  getCatalog,
  getTableDdl,
  pickFile,
  pickFolder,
  refreshCatalog,
  testConnectionProfile,
  upsertConnection,
} from "../../ipc/commands";
import type {
  Catalog,
  CatalogTable,
  ConnectionProfile,
  Engine,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { tableKey, tableLabel } from "../../lib/tableRef";
import ConfirmButton from "../../components/ConfirmButton";
import { Icon } from "../../components/Icon";
import SqlViewer from "../../components/SqlViewer";
import { useToast } from "../../components/Toast";
import "./connections.css";

const DEFAULT_PORT: Record<Engine, number> = {
  postgres: 5432,
  mysql: 3306,
  sqlite: 0,
};

function blank(): ConnectionProfile {
  return {
    id: crypto.randomUUID(),
    name: "",
    engine: "postgres",
    host: "localhost",
    port: 5432,
    database: "",
    username: "",
    sslmode: "prefer",
    extraParams: {},
    readonlyDefault: true,
    allowWrites: false,
    secretRef: null,
    projectDir: null,
    env: null,
  };
}

// The CREATE-TABLE DDL modal: monospace read-only view, Copy button, Esc/overlay closes.
function DdlModal({
  conn,
  table,
  onClose,
}: {
  conn: ConnectionProfile;
  table: CatalogTable;
  onClose: () => void;
}) {
  const [text, setText] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const closeRef = useRef<HTMLButtonElement>(null);

  // Focus the Close button on open and restore focus to the trigger on close so
  // keyboard/SR users aren't left Tabbing behind the modal.
  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    return () => trigger?.focus?.();
  }, []);

  useEffect(() => {
    let alive = true;
    getTableDdl(conn.id, table.name, table.schema)
      .then((d) => alive && setText(d))
      .catch((e) => alive && setErr(errMessage(e)));
    return () => {
      alive = false;
    };
  }, [conn.id, table.name, table.schema]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  async function copy() {
    if (!text) return;
    await navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  return (
    <div className="ddl-overlay" onClick={onClose}>
      <div
        className="ddl-modal"
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="ddl-head">
          <span className="ddl-title">{tableLabel(conn.engine, table)} — DDL</span>
          <div className="ddl-actions">
            <button className="btn small" onClick={copy} disabled={!text}>
              {copied ? "Copied" : "Copy"}
            </button>
            <button className="btn small" ref={closeRef} onClick={onClose}>
              Close
            </button>
          </div>
        </div>
        {err && <div className="error">{err}</div>}
        {!err && text == null && <div className="muted small-pad loading">Loading…</div>}
        {text != null && <SqlViewer value={text} minHeight="240px" />}
      </div>
    </div>
  );
}

// DataGrip-style Database Explorer: connections in the sidebar, the selected one
// expanded to reveal its tables. Clicking a table opens its data in the main area.
export function DatabaseExplorer({
  connections,
  selectedId,
  selectedTableKey,
  onSelectConn,
  onOpenTable,
  onOpenMigrations,
  onNew,
  onOpenSettings,
  migrationsOpen,
}: {
  connections: ConnectionProfile[];
  selectedId: string | null;
  selectedTableKey: string | null;
  onSelectConn: (id: string) => void;
  onOpenTable: (conn: ConnectionProfile, table: CatalogTable) => void;
  onOpenMigrations: (conn: ConnectionProfile) => void;
  onNew: () => void;
  onOpenSettings: () => void;
  migrationsOpen: boolean;
}) {
  // Per-connection: any node can be expanded independently of selection, so
  // catalogs/errors/filters are keyed by connection id (DataGrip-style tree).
  const [catalogs, setCatalogs] = useState<Record<string, Catalog>>({});
  const [errs, setErrs] = useState<Record<string, string>>({});
  const [filters, setFilters] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [refreshing, setRefreshing] = useState<string | null>(null);
  const [tablesOpen, setTablesOpen] = useState(true);
  const [viewsOpen, setViewsOpen] = useState(true);
  const [ddl, setDdl] = useState<{ conn: ConnectionProfile; table: CatalogTable } | null>(
    null,
  );
  const loadedRef = useRef(new Set<string>());

  function ensureLoaded(id: string) {
    if (loadedRef.current.has(id)) return;
    loadedRef.current.add(id);
    setErrs((m) => {
      const n = { ...m };
      delete n[id];
      return n;
    });
    getCatalog(id)
      .then((c) => setCatalogs((m) => ({ ...m, [id]: c })))
      .catch((e) => {
        loadedRef.current.delete(id); // allow retry on next expand
        setErrs((m) => ({ ...m, [id]: errMessage(e) }));
      });
  }

  function toggleOpen(id: string) {
    const willOpen = !open.has(id);
    setOpen((o) => {
      const n = new Set(o);
      if (willOpen) n.add(id);
      else n.delete(id);
      return n;
    });
    if (willOpen) ensureLoaded(id);
  }

  // Selecting a connection auto-expands it (collapse stays a free action after).
  useEffect(() => {
    if (!selectedId) return;
    setOpen((o) => (o.has(selectedId) ? o : new Set(o).add(selectedId)));
    ensureLoaded(selectedId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  // Force a live re-introspection — the schema cache is written once and never
  // expires, so a table list can go stale (e.g. tables added after first connect).
  async function refreshSchema(id: string) {
    setRefreshing(id);
    setErrs((m) => {
      const n = { ...m };
      delete n[id];
      return n;
    });
    try {
      const c = await refreshCatalog(id);
      setCatalogs((m) => ({ ...m, [id]: c }));
      loadedRef.current.add(id);
    } catch (e) {
      setErrs((m) => ({ ...m, [id]: errMessage(e) }));
    } finally {
      setRefreshing(null);
    }
  }

  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <span>Databases</span>
        <button className="btn small" onClick={onNew}>
          + New
        </button>
      </div>
      <div className="explorer">
        {connections.length === 0 && (
          <div className="muted empty">No connections yet.</div>
        )}
        {connections.map((c) => {
          const isSel = c.id === selectedId;
          return (
            <div key={c.id} className="db-node">
              <div
                className={isSel ? "db-conn selected" : "db-conn"}
                role="button"
                tabIndex={0}
                onClick={() => (isSel ? toggleOpen(c.id) : onSelectConn(c.id))}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    if (isSel) toggleOpen(c.id);
                    else onSelectConn(c.id);
                  }
                }}
                title={`${c.engine} · ${c.host}${
                  c.engine !== "sqlite" ? `:${c.port}` : ""
                } · ${c.database}`}
              >
                <span
                  className="tw"
                  title={open.has(c.id) ? "Collapse" : "Expand"}
                  onClick={(e) => {
                    e.stopPropagation();
                    toggleOpen(c.id);
                  }}
                >
                  <Icon name={open.has(c.id) ? "chevronDown" : "chevronRight"} />
                </span>
                <span className="db-conn-name">{c.name || "(unnamed)"}</span>
                {(c.env === "staging" || c.env === "prod") && (
                  <span className={`env-chip env-${c.env}`}>{c.env}</span>
                )}
                <span className="db-conn-engine muted">{c.engine}</span>
                {open.has(c.id) && (
                  <button
                    className="db-refresh"
                    title="Refresh schema (re-introspect live)"
                    aria-label="Refresh schema"
                    onClick={(e) => {
                      e.stopPropagation();
                      void refreshSchema(c.id);
                    }}
                  >
                    {refreshing === c.id ? "…" : <Icon name="refresh" />}
                  </button>
                )}
              </div>

              {open.has(c.id) &&
                (() => {
                  const cat = catalogs[c.id];
                  const cerr = errs[c.id];
                  const filter = filters[c.id] ?? "";
                  const f = filter.trim().toLowerCase();
                  const all = cat
                    ? f
                      ? cat.tables.filter((t) => t.name.toLowerCase().includes(f))
                      : cat.tables
                    : [];
                  const tbls = all.filter((t) => t.kind !== "view");
                  const views = all.filter((t) => t.kind === "view");
                  const renderRow = (t: CatalogTable) => {
                    const key = tableKey(t);
                    return (
                      <div
                        key={key}
                        className={
                          isSel && selectedTableKey === key
                            ? "db-table selected"
                            : "db-table"
                        }
                        role="button"
                        tabIndex={0}
                        onClick={() => onOpenTable(c, t)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            onOpenTable(c, t);
                          }
                        }}
                        title={`${t.columns.length} columns`}
                      >
                        <span className="db-table-ico">
                          {t.kind === "view" ? "◇" : "▦"}
                        </span>
                        <span className="tbl-name">
                          {tableLabel(c.engine, t)}
                        </span>
                        {t.rowEstimate != null && t.rowEstimate >= 0 && (
                          <span className="tbl-count muted">
                            ~{t.rowEstimate.toLocaleString()}
                          </span>
                        )}
                        <button
                          className="ddl-btn"
                          title="Show CREATE DDL"
                          onClick={(e) => {
                            e.stopPropagation();
                            setDdl({ conn: c, table: t });
                          }}
                        >
                          DDL
                        </button>
                      </div>
                    );
                  };
                  return (
                    <div className="db-tables">
                      <div
                        className={migrationsOpen && isSel ? "db-nav active" : "db-nav"}
                        role="button"
                        tabIndex={0}
                        onClick={() => onOpenMigrations(c)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            onOpenMigrations(c);
                          }
                        }}
                        title="Schema migration change-log"
                      >
                        <span className="db-nav-ico">◱</span> Migrations
                      </div>
                      {cat && cat.tables.length > 5 && (
                        <input
                          className="table-filter"
                          placeholder="Filter tables…"
                          value={filter}
                          onChange={(e) =>
                            setFilters((m) => ({ ...m, [c.id]: e.target.value }))
                          }
                        />
                      )}
                      {cerr && <div className="error small-pad">{cerr}</div>}
                      {!cat && !cerr && (
                        <div className="muted small-pad loading">Loading schema…</div>
                      )}
                      {cat && all.length === 0 && (
                        <div className="muted small-pad">
                          {f ? `No tables match "${f}".` : "No tables."}
                        </div>
                      )}
                      {tbls.length > 0 && (
                        <>
                          <div
                            className="db-section"
                            role="button"
                            tabIndex={0}
                            aria-expanded={tablesOpen}
                            onClick={() => setTablesOpen((o) => !o)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter" || e.key === " ") {
                                e.preventDefault();
                                setTablesOpen((o) => !o);
                              }
                            }}
                          >
                            <span className="tw">
                              <Icon name={tablesOpen ? "chevronDown" : "chevronRight"} />
                            </span>{" "}
                            Tables ({tbls.length})
                          </div>
                          {tablesOpen && tbls.map(renderRow)}
                        </>
                      )}
                      {views.length > 0 && (
                        <>
                          <div
                            className="db-section"
                            role="button"
                            tabIndex={0}
                            aria-expanded={viewsOpen}
                            onClick={() => setViewsOpen((o) => !o)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter" || e.key === " ") {
                                e.preventDefault();
                                setViewsOpen((o) => !o);
                              }
                            }}
                          >
                            <span className="tw">
                              <Icon name={viewsOpen ? "chevronDown" : "chevronRight"} />
                            </span>{" "}
                            Views ({views.length})
                          </div>
                          {viewsOpen && views.map(renderRow)}
                        </>
                      )}
                    </div>
                  );
                })()}
            </div>
          );
        })}
      </div>

      <div className="sidebar-foot">
        <button className="foot-btn" onClick={onOpenSettings}>
          <span className="gear"><Icon name="gear" /></span> Settings
        </button>
      </div>

      {ddl && (
        <DdlModal conn={ddl.conn} table={ddl.table} onClose={() => setDdl(null)} />
      )}
    </aside>
  );
}

export function ConnectionForm({
  initial,
  onSaved,
  onCancel,
  onDeleted,
}: {
  initial: ConnectionProfile | null;
  onSaved: (p: ConnectionProfile) => void;
  onCancel: () => void;
  onDeleted: (id: string) => void;
}) {
  const toast = useToast();
  const [form, setForm] = useState<ConnectionProfile>(initial ?? blank());
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  // Which action is in flight, so only the clicked button shows progress (busy
  // disables all three).
  const [running, setRunning] = useState<"save" | "test" | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgErr, setMsgErr] = useState(false);
  const isNew = initial === null;

  function set<K extends keyof ConnectionProfile>(
    key: K,
    value: ConnectionProfile[K],
  ) {
    setForm((f) => ({ ...f, [key]: value }));
  }

  async function save() {
    setBusy(true);
    setRunning("save");
    setMsg(null);
    try {
      const saved = await upsertConnection(form, password || undefined);
      setPassword("");
      toast("Connection saved");
      onSaved(saved);
      setMsg("Saved.");
      setMsgErr(false);
    } catch (e) {
      setMsg(errMessage(e));
      setMsgErr(true);
    } finally {
      setBusy(false);
      setRunning(null);
    }
  }

  async function test() {
    setBusy(true);
    setRunning("test");
    setMsg(null);
    try {
      // A literal reachability check — dials the current form values WITHOUT
      // saving the connection or storing the secret. Just OK / not OK.
      await testConnectionProfile(form, password || undefined);
      setMsg("✓ Connection OK");
      setMsgErr(false);
    } catch (e) {
      setMsg(errMessage(e));
      setMsgErr(true);
    } finally {
      setBusy(false);
      setRunning(null);
    }
  }

  async function remove() {
    setBusy(true);
    try {
      await deleteConnection(form.id);
      toast("Connection deleted");
      onDeleted(form.id);
    } catch (e) {
      const m = errMessage(e);
      setMsg(m);
      setMsgErr(true);
      toast(m, "error");
    } finally {
      setBusy(false);
    }
  }

  const isSqlite = form.engine === "sqlite";

  return (
    <div
      className="form"
      onKeyDown={(e) => {
        if (
          e.key === "Enter" &&
          (e.target as HTMLElement).tagName === "INPUT" &&
          !busy
        ) {
          e.preventDefault();
          void save();
        } else if (e.key === "Escape") {
          onCancel();
        }
      }}
    >
      <h2>{isNew ? "New connection" : "Edit connection"}</h2>

      <label>
        Name
        <input
          value={form.name}
          onChange={(e) => set("name", e.target.value)}
          placeholder="prod-readonly"
        />
      </label>

      <label>
        Engine
        <select
          value={form.engine}
          onChange={(e) => {
            const engine = e.target.value as Engine;
            setForm((f) => ({
              ...f,
              engine,
              // Keep a user-customized port; only swap when it still matches the
              // outgoing engine's default.
              port: f.port === DEFAULT_PORT[f.engine] ? DEFAULT_PORT[engine] : f.port,
            }));
          }}
        >
          <option value="postgres">PostgreSQL</option>
          <option value="mysql">MySQL / MariaDB</option>
          <option value="sqlite">SQLite</option>
        </select>
      </label>

      {isSqlite ? (
        <label>
          Database file path
          <div className="row">
            <input
              className="grow"
              value={form.database}
              onChange={(e) => set("database", e.target.value)}
              placeholder="/path/to/app.db"
            />
            <button
              type="button"
              className="btn small"
              onClick={() => void pickFile().then((f) => f && set("database", f))}
            >
              Browse…
            </button>
          </div>
        </label>
      ) : (
        <>
          <div className="row">
            <label className="grow">
              Host
              <input
                value={form.host}
                onChange={(e) => set("host", e.target.value)}
              />
            </label>
            <label className="port">
              Port
              <input
                type="number"
                value={form.port}
                onChange={(e) => {
                  // Empty input keeps the previous port instead of silently becoming 0.
                  const v = e.target.value;
                  if (v !== "") set("port", Number(v));
                }}
              />
            </label>
          </div>

          <label>
            Database
            <input
              value={form.database}
              onChange={(e) => set("database", e.target.value)}
            />
          </label>

          <div className="row">
            <label className="grow">
              User
              <input
                value={form.username}
                onChange={(e) => set("username", e.target.value)}
              />
            </label>
            <label className="grow">
              Password
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={
                  form.secretRef ? "•••••• (stored)" : "stored in Keychain"
                }
              />
            </label>
          </div>

          <label>
            SSL mode
            <select
              value={form.sslmode}
              onChange={(e) => set("sslmode", e.target.value)}
            >
              <option value="disable">disable</option>
              <option value="prefer">prefer</option>
              <option value="require">require</option>
              <option value="verify-full">verify-full</option>
            </select>
          </label>
        </>
      )}

      <label>
        Project folder <span className="muted">(optional — locates migrations)</span>
        <div className="row">
          <input
            className="grow"
            value={form.projectDir ?? ""}
            onChange={(e) => set("projectDir", e.target.value || null)}
            placeholder="/path/to/your/project"
          />
          <button
            type="button"
            className="btn small"
            onClick={() => void pickFolder().then((d) => d && set("projectDir", d))}
          >
            Browse…
          </button>
        </div>
      </label>

      <label>
        Environment <span className="muted">(optional — labels the sidebar)</span>
        <select
          value={form.env ?? ""}
          onChange={(e) => set("env", e.target.value || null)}
        >
          <option value="">— none —</option>
          <option value="dev">dev</option>
          <option value="staging">staging</option>
          <option value="prod">prod</option>
        </select>
      </label>

      <p className="muted">
        Write access is controlled per-connection in Settings ▸ Safety.
      </p>

      <div className="form-actions">
        <button className="btn primary" disabled={busy} onClick={save}>
          {running === "save" ? "Saving…" : "Save"}
        </button>
        <button className="btn" disabled={busy} onClick={test}>
          {running === "test" ? "Testing…" : "Test connection"}
        </button>
        <button className="btn" disabled={busy} onClick={onCancel}>
          Cancel
        </button>
        {!isNew && (
          <ConfirmButton className="btn danger" disabled={busy} onConfirm={remove}>
            Delete
          </ConfirmButton>
        )}
      </div>

      {msg && (
        <div className={msgErr ? "form-msg error" : "form-msg ok"}>{msg}</div>
      )}
    </div>
  );
}
