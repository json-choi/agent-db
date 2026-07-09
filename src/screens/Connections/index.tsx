// Connection sidebar list + create/edit form. Secrets are never held here after
// save — the password is handed to the backend, which stores it in the OS credential store.
import { useEffect, useMemo, useRef, useState, type PointerEvent } from "react";
import {
  deleteConnection,
  getCatalog,
  getTableDdl,
  pickFile,
  pickFolder,
  refreshCatalog,
  setConnectionSchemaGroup,
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
import {
  buildConnectionSections,
  compareCatalogs,
  diffCounts,
  tableDiffTone,
  type SchemaConnectionGroup,
  type SchemaDiffSummary,
  type TableSchemaDiff,
} from "../../lib/schemaDiff";
import { tableKey, tableLabel } from "../../lib/tableRef";
import ConfirmButton from "../../components/ConfirmButton";
import EngineMark from "../../components/EngineMark";
import { Icon } from "../../components/Icon";
import InfoTip from "../../components/InfoTip";
import LazySqlViewer from "../../components/LazySqlViewer";
import { useToast } from "../../components/Toast";
import { useI18n } from "../../lib/i18n";
import "./connections.css";

const DEFAULT_PORT: Record<Engine, number> = {
  postgres: 5432,
  mysql: 3306,
  sqlite: 0,
};
const SCHEMA_LOAD_TIMEOUT_MS = 12_000;

type DropTarget =
  | { kind: "connection"; id: string }
  | { kind: "group"; key: string };

type DragStart = {
  id: string;
  pointerId: number;
  x: number;
  y: number;
};

function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  let timer: number | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = window.setTimeout(() => reject(new Error(message)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => window.clearTimeout(timer));
}

function stripEnvTokens(value: string): string {
  return value
    .replace(/\b(development|staging|production|local|dev|stage|prod|qa|test)\b/gi, "")
    .replace(/(^|[-_.\s]+)(development|staging|production|local|dev|stage|prod|qa|test)([-_.\s]+|$)/gi, "$1")
    .replace(/[-_.\s]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .trim();
}

function fallbackSchemaGroupName(
  a: ConnectionProfile,
  b: ConnectionProfile,
  connections: ConnectionProfile[],
): string {
  const candidates = [
    stripEnvTokens(a.name),
    stripEnvTokens(b.name),
    stripEnvTokens(a.database),
    stripEnvTokens(b.database),
    stripEnvTokens(a.host.split(".")[0] ?? ""),
    stripEnvTokens(b.host.split(".")[0] ?? ""),
  ].filter(Boolean);
  const base = candidates.find((candidate) => candidate.length >= 2) ?? "schema-group";
  const used = new Set(
    connections
      .map((conn) => conn.schemaGroup?.trim().toLocaleLowerCase())
      .filter(Boolean) as string[],
  );
  if (!used.has(base.toLocaleLowerCase())) return base;
  let suffix = 2;
  while (used.has(`${base}-${suffix}`.toLocaleLowerCase())) suffix += 1;
  return `${base}-${suffix}`;
}

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
    schemaGroup: null,
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
  const { t } = useI18n();
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
          <span className="ddl-title">
            {t("connections.ddlTitle", { table: tableLabel(conn.engine, table) })}
          </span>
          <div className="ddl-actions">
            <button className="btn small" onClick={copy} disabled={!text}>
              {copied ? t("common.copied") : t("common.copy")}
            </button>
            <button className="btn small" ref={closeRef} onClick={onClose}>
              {t("common.close")}
            </button>
          </div>
        </div>
        {err && <div className="error">{err}</div>}
        {!err && text == null && (
          <div className="muted small-pad loading">{t("common.loading")}</div>
        )}
        {text != null && <LazySqlViewer value={text} minHeight="240px" />}
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
  onEdit,
  onDeleted,
  onConnectionUpdated,
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
  onEdit: (conn: ConnectionProfile) => void;
  onDeleted: (id: string) => void;
  onConnectionUpdated: (conn: ConnectionProfile) => void;
  onOpenSettings: () => void;
  migrationsOpen: boolean;
}) {
  const { t } = useI18n();
  const toast = useToast();
  // Per-connection: any node can be expanded independently of selection, so
  // catalogs/errors/filters are keyed by connection id (DataGrip-style tree).
  const [catalogs, setCatalogs] = useState<Record<string, Catalog>>({});
  const [errs, setErrs] = useState<Record<string, string>>({});
  const [filters, setFilters] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [refreshing, setRefreshing] = useState<string | null>(null);
  const [tablesOpen, setTablesOpen] = useState(true);
  const [viewsOpen, setViewsOpen] = useState(true);
  const [showRowCounts, setShowRowCounts] = useState(true);
  const [ddl, setDdl] = useState<{ conn: ConnectionProfile; table: CatalogTable } | null>(
    null,
  );
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);
  const [dragPreview, setDragPreview] = useState<{ id: string; x: number; y: number } | null>(
    null,
  );
  const loadedRef = useRef(new Set<string>());
  const dragStartRef = useRef<DragStart | null>(null);
  const activeDragIdRef = useRef<string | null>(null);
  const suppressClickRef = useRef(false);
  const sections = useMemo(() => buildConnectionSections(connections), [connections]);
  const groupByConnectionId = useMemo(() => {
    const map = new Map<string, SchemaConnectionGroup>();
    for (const section of sections) {
      if (section.kind !== "group") continue;
      for (const conn of section.group.connections) map.set(conn.id, section.group);
    }
    return map;
  }, [sections]);

  function ensureLoaded(id: string) {
    if (loadedRef.current.has(id)) return;
    loadedRef.current.add(id);
    setErrs((m) => {
      const n = { ...m };
      delete n[id];
      return n;
    });
    withTimeout(
      getCatalog(id),
      SCHEMA_LOAD_TIMEOUT_MS,
      "Schema loading timed out. Check the database connection or retry.",
    )
      .then((c) => setCatalogs((m) => ({ ...m, [id]: c })))
      .catch((e) => {
        loadedRef.current.delete(id); // allow retry on next expand
        setErrs((m) => ({ ...m, [id]: errMessage(e) }));
      });
  }

  function ensureGroupLoaded(id: string) {
    const group = groupByConnectionId.get(id);
    if (!group) {
      ensureLoaded(id);
      return;
    }
    for (const conn of group.connections) ensureLoaded(conn.id);
  }

  function toggleOpen(id: string) {
    const willOpen = !open.has(id);
    setOpen((o) => {
      const n = new Set(o);
      if (willOpen) n.add(id);
      else n.delete(id);
      return n;
    });
    if (willOpen) ensureGroupLoaded(id);
  }

  // Selecting a connection auto-expands it (collapse stays a free action after).
  useEffect(() => {
    if (!selectedId) return;
    setOpen((o) => (o.has(selectedId) ? o : new Set(o).add(selectedId)));
    ensureGroupLoaded(selectedId);
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
      const c = await withTimeout(
        refreshCatalog(id),
        SCHEMA_LOAD_TIMEOUT_MS,
        "Schema refresh timed out. Check the database connection or retry.",
      );
      setCatalogs((m) => ({ ...m, [id]: c }));
      loadedRef.current.add(id);
    } catch (e) {
      setErrs((m) => ({ ...m, [id]: errMessage(e) }));
    } finally {
      setRefreshing(null);
    }
  }

  async function removeConnection(conn: ConnectionProfile) {
    try {
      await deleteConnection(conn.id);
      toast(t("connections.connectionDeleted"));
      onDeleted(conn.id);
    } catch (e) {
      toast(errMessage(e), "error");
    }
  }

  function connectionById(id: string) {
    return connections.find((conn) => conn.id === id) ?? null;
  }

  function schemaGroupByKey(key: string) {
    for (const section of sections) {
      if (section.kind === "group" && section.group.key === key) {
        return section.group;
      }
    }
    return null;
  }

  function canDropOnConnection(dragId: string | null, target: ConnectionProfile) {
    return !!dragId && dragId !== target.id && !!connectionById(dragId);
  }

  function canDropOnGroup(dragId: string | null, group: SchemaConnectionGroup) {
    return !!dragId && !group.connections.some((conn) => conn.id === dragId);
  }

  async function saveSchemaGroupUpdates(updates: Array<{ id: string; group: string }>) {
    const originals = updates
      .map((update) => connectionById(update.id))
      .filter((conn): conn is ConnectionProfile => !!conn);
    for (const update of updates) {
      const original = connectionById(update.id);
      if (original) onConnectionUpdated({ ...original, schemaGroup: update.group });
    }
    try {
      const saved = await Promise.all(
        updates.map((update) => setConnectionSchemaGroup(update.id, update.group)),
      );
      saved.forEach(onConnectionUpdated);
    } catch (err) {
      originals.forEach(onConnectionUpdated);
      throw err;
    }
  }

  async function groupDraggedWithConnection(
    dragged: ConnectionProfile,
    target: ConnectionProfile,
  ) {
    if (!canDropOnConnection(dragged.id, target)) return;
    const targetGroup = target.schemaGroup?.trim();
    const draggedGroup = dragged.schemaGroup?.trim();
    const group =
      targetGroup ||
      draggedGroup ||
      fallbackSchemaGroupName(dragged, target, connections);
    const updates = [
      ...(draggedGroup === group ? [] : [{ id: dragged.id, group }]),
      ...(targetGroup === group ? [] : [{ id: target.id, group }]),
    ];
    if (updates.length === 0) return;
    const confirmed = window.confirm(
      t("connections.schemaGroupConfirmPair", {
        source: dragged.name || dragged.database || t("app.unnamed"),
        target: target.name || target.database || t("app.unnamed"),
        group,
      }),
    );
    if (!confirmed) return;
    try {
      await saveSchemaGroupUpdates(updates);
      toast(t("connections.schemaGroupUpdated"));
    } catch (err) {
      toast(errMessage(err), "error");
    }
  }

  async function groupDraggedIntoGroup(
    dragged: ConnectionProfile,
    group: SchemaConnectionGroup,
  ) {
    if (!canDropOnGroup(dragged.id, group)) return;
    if (dragged.schemaGroup?.trim() === group.label) return;
    const confirmed = window.confirm(
      t("connections.schemaGroupConfirmGroup", {
        connection: dragged.name || dragged.database || t("app.unnamed"),
        group: group.label,
      }),
    );
    if (!confirmed) return;
    try {
      await saveSchemaGroupUpdates([{ id: dragged.id, group: group.label }]);
      toast(t("connections.schemaGroupUpdated"));
    } catch (err) {
      toast(errMessage(err), "error");
    }
  }

  async function confirmAndApplyDrop(dragId: string, target: DropTarget) {
    const dragged = connectionById(dragId);
    if (!dragged) return;
    if (target.kind === "connection") {
      const targetConn = connectionById(target.id);
      if (!targetConn) return;
      await groupDraggedWithConnection(dragged, targetConn);
      return;
    }
    const group = schemaGroupByKey(target.key);
    if (group) await groupDraggedIntoGroup(dragged, group);
  }

  function isInteractiveDragTarget(target: EventTarget | null) {
    return (
      target instanceof HTMLElement &&
      !!target.closest(
        "button,input,select,textarea,a,summary,details,.db-menu,.tw,.ddl-btn",
      )
    );
  }

  function sameDropTarget(a: DropTarget | null, b: DropTarget | null) {
    if (!a || !b) return a === b;
    if (a.kind !== b.kind) return false;
    if (a.kind === "connection" && b.kind === "connection") return a.id === b.id;
    if (a.kind === "group" && b.kind === "group") return a.key === b.key;
    return false;
  }

  function dropTargetFromPoint(dragId: string, x: number, y: number): DropTarget | null {
    const element = document.elementFromPoint(x, y);
    if (!(element instanceof HTMLElement)) return null;

    const connectionEl = element.closest<HTMLElement>("[data-connection-id]");
    const targetConnId = connectionEl?.dataset.connectionId;
    if (targetConnId) {
      const targetConn = connectionById(targetConnId);
      if (targetConn && canDropOnConnection(dragId, targetConn)) {
        return { kind: "connection", id: targetConn.id };
      }
    }

    const groupEl = element.closest<HTMLElement>("[data-schema-group-key]");
    const targetGroupKey = groupEl?.dataset.schemaGroupKey;
    if (targetGroupKey) {
      const group = schemaGroupByKey(targetGroupKey);
      if (group && canDropOnGroup(dragId, group)) {
        return { kind: "group", key: group.key };
      }
    }

    return null;
  }

  function updateDropTargetFromPoint(dragId: string, x: number, y: number) {
    const next = dropTargetFromPoint(dragId, x, y);
    setDropTarget((current) => (sameDropTarget(current, next) ? current : next));
  }

  function clearPointerDrag() {
    dragStartRef.current = null;
    activeDragIdRef.current = null;
    setDraggingId(null);
    setDropTarget(null);
    setDragPreview(null);
  }

  function pointerDownConnection(e: PointerEvent<HTMLDivElement>, conn: ConnectionProfile) {
    if (e.button !== 0 || isInteractiveDragTarget(e.target)) return;
    dragStartRef.current = {
      id: conn.id,
      pointerId: e.pointerId,
      x: e.clientX,
      y: e.clientY,
    };
    e.currentTarget.setPointerCapture?.(e.pointerId);
  }

  function pointerMoveConnection(e: PointerEvent<HTMLDivElement>) {
    const start = dragStartRef.current;
    if (!start || start.pointerId !== e.pointerId) return;
    const distance = Math.hypot(e.clientX - start.x, e.clientY - start.y);
    if (!activeDragIdRef.current && distance < 6) return;
    if (!activeDragIdRef.current) {
      activeDragIdRef.current = start.id;
      suppressClickRef.current = true;
      setDraggingId(start.id);
    }
    e.preventDefault();
    setDragPreview({ id: start.id, x: e.clientX, y: e.clientY });
    updateDropTargetFromPoint(start.id, e.clientX, e.clientY);
  }

  function pointerUpConnection(e: PointerEvent<HTMLDivElement>) {
    const activeId = activeDragIdRef.current;
    const target = activeId ? dropTargetFromPoint(activeId, e.clientX, e.clientY) : null;
    if (dragStartRef.current?.pointerId === e.pointerId) {
      try {
        e.currentTarget.releasePointerCapture?.(e.pointerId);
      } catch {
        // Pointer capture may already be released by the browser.
      }
    }
    clearPointerDrag();
    if (activeId) window.setTimeout(() => {
      suppressClickRef.current = false;
    }, 0);
    if (activeId && target) void confirmAndApplyDrop(activeId, target);
  }

  function pointerCancelConnection(e: PointerEvent<HTMLDivElement>) {
    if (dragStartRef.current?.pointerId === e.pointerId) {
      try {
        e.currentTarget.releasePointerCapture?.(e.pointerId);
      } catch {
        // Pointer capture may already be released by the browser.
      }
    }
    clearPointerDrag();
  }

  function prodBaseline(group: SchemaConnectionGroup): ConnectionProfile | null {
    return group.connections.find((conn) => conn.env === "prod") ?? null;
  }

  function schemaDiffForConnection(conn: ConnectionProfile): SchemaDiffSummary | null {
    const group = groupByConnectionId.get(conn.id);
    if (!group) return null;
    const baseline = prodBaseline(group);
    if (!baseline || baseline.id === conn.id) return null;
    const current = catalogs[conn.id];
    const prod = catalogs[baseline.id];
    if (!current || !prod) return null;
    return compareCatalogs(current, prod);
  }

  function schemaDiffTitle(diff: SchemaDiffSummary) {
    const counts = diffCounts(diff);
    return t("connections.schemaDiffTitle", {
      added: counts.added,
      missing: counts.missing,
      changed: counts.changed,
    });
  }

  function tableDiffTitle(diff: TableSchemaDiff) {
    if (diff.added) return t("connections.schemaDiffTableAdded");
    if (diff.missing) return t("connections.schemaDiffTableMissing");
    return t("connections.schemaDiffTableChanged", {
      added: diff.addedColumns.length,
      missing: diff.missingColumns.length,
      changed: diff.changedColumns.length + (diff.relationChanged ? 1 : 0),
    });
  }

  function renderSchemaDiffBadge(conn: ConnectionProfile) {
    const group = groupByConnectionId.get(conn.id);
    if (!group) return null;
    const baseline = prodBaseline(group);
    if (!baseline || baseline.id === conn.id) return null;
    const current = catalogs[conn.id];
    const prod = catalogs[baseline.id];
    if (!current || !prod) {
      return (
        <span className="schema-diff-chip diff-pending" title={t("connections.schemaDiffPendingTitle")}>
          {t("connections.schemaDiffPendingChip")}
        </span>
      );
    }
    const diff = compareCatalogs(current, prod);
    if (diff.total === 0) {
      return (
        <span className="schema-diff-chip diff-ok" title={t("connections.schemaDiffInSync")}>
          <Icon name="check" />
        </span>
      );
    }
    const counts = diffCounts(diff);
    return (
      <span className="schema-diff-chip diff-drift" title={schemaDiffTitle(diff)}>
        {counts.added > 0 && <span className="diff-add">+{counts.added}</span>}
        {counts.missing > 0 && <span className="diff-remove">-{counts.missing}</span>}
        {counts.changed > 0 && <span className="diff-change">~{counts.changed}</span>}
      </span>
    );
  }

  function tableMatchesFilter(table: CatalogTable, f: string) {
    return (
      table.name.toLowerCase().includes(f) ||
      (table.schema ?? "").toLowerCase().includes(f)
    );
  }

  function renderConnection(c: ConnectionProfile, nested = false) {
    const isSel = c.id === selectedId;
    const isDropTarget =
      dropTarget?.kind === "connection" && dropTarget.id === c.id;
    const rowClass = [
      "db-conn",
      "ds-object-row",
      isSel ? "selected" : "",
      nested ? "nested" : "",
      draggingId === c.id ? "dragging" : "",
      isDropTarget ? "drop-target" : "",
    ]
      .filter(Boolean)
      .join(" ");

    return (
      <div key={c.id} className="db-node">
        <div
          data-connection-id={c.id}
          className={rowClass}
          role="button"
          tabIndex={0}
          onPointerDown={(e) => pointerDownConnection(e, c)}
          onPointerMove={pointerMoveConnection}
          onPointerUp={pointerUpConnection}
          onPointerCancel={pointerCancelConnection}
          onClick={() => {
            if (suppressClickRef.current) {
              suppressClickRef.current = false;
              return;
            }
            if (isSel) toggleOpen(c.id);
            else onSelectConn(c.id);
          }}
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
            title={open.has(c.id) ? t("connections.collapse") : t("connections.expand")}
            onClick={(e) => {
              e.stopPropagation();
              toggleOpen(c.id);
            }}
          >
            <Icon name={open.has(c.id) ? "chevronDown" : "chevronRight"} />
          </span>
          {!nested && <EngineMark engine={c.engine} />}
          <span className="db-conn-name">{c.name || t("app.unnamed")}</span>
          {c.env && <span className={`env-chip env-${c.env}`}>{c.env}</span>}
          {renderSchemaDiffBadge(c)}
          <details className="db-menu" onClick={(e) => e.stopPropagation()}>
            <summary
              className="db-menu-trigger"
              title={t("connections.connectionMenu")}
              aria-label={t("connections.connectionMenu")}
            >
              <Icon name="gear" />
            </summary>
            <div className="db-menu-panel">
              <button type="button" onClick={() => onEdit(c)}>
                {t("connections.edit")}
              </button>
              <button type="button" onClick={() => void refreshSchema(c.id)}>
                {refreshing === c.id ? t("mcp.working") : t("connections.refreshSchema")}
              </button>
              <label>
                <input
                  type="checkbox"
                  checked={showRowCounts}
                  onChange={(e) => setShowRowCounts(e.target.checked)}
                />
                {t("connections.showRowCounts")}
              </label>
              <ConfirmButton
                className="db-menu-item danger"
                confirmLabel={t("common.reallyDelete")}
                onConfirm={() => void removeConnection(c)}
              >
                {t("common.delete")}
              </ConfirmButton>
            </div>
          </details>
        </div>

        {open.has(c.id) &&
          (() => {
            const cat = catalogs[c.id];
            const cerr = errs[c.id];
            const diff = schemaDiffForConnection(c);
            const filter = filters[c.id] ?? "";
            const f = filter.trim().toLowerCase();
            const all = cat
              ? f
                ? cat.tables.filter((t) => tableMatchesFilter(t, f))
                : cat.tables
              : [];
            const missingTables = diff
              ? f
                ? diff.missingTables.filter((t) => tableMatchesFilter(t, f))
                : diff.missingTables
              : [];
            const tbls = all.filter((t) => t.kind !== "view");
            const views = all.filter((t) => t.kind === "view");
            const renderRow = (table: CatalogTable) => {
              const key = tableKey(table);
              const tableDiff = diff?.tableDiffs[key];
              const tone = tableDiffTone(tableDiff);
              const rowClasses = [
                "db-table",
                "ds-object-row",
                isSel && selectedTableKey === key ? "selected" : "",
                tone ? `schema-diff-${tone}` : "",
              ]
                .filter(Boolean)
                .join(" ");
              return (
                <div
                  key={key}
                  className={rowClasses}
                  aria-selected={isSel && selectedTableKey === key}
                  role="button"
                  tabIndex={0}
                  onClick={() => onOpenTable(c, table)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      onOpenTable(c, table);
                    }
                  }}
                  title={tableDiff ? tableDiffTitle(tableDiff) : t("connections.columns", { count: table.columns.length })}
                >
                  {tone && (
                    <span
                      className={`schema-diff-dot diff-${tone}`}
                      title={tableDiff ? tableDiffTitle(tableDiff) : undefined}
                    />
                  )}
                  <span className="db-table-ico">
                    {table.kind === "view" ? "◇" : "▦"}
                  </span>
                  <span className="tbl-name">
                    {tableLabel(c.engine, table)}
                  </span>
                  {showRowCounts && table.rowEstimate != null && table.rowEstimate >= 0 && (
                    <span className="tbl-count muted">
                      ~{table.rowEstimate.toLocaleString()}
                    </span>
                  )}
                  <button
                    className="ddl-btn"
                    title={t("connections.showDdl")}
                    onClick={(e) => {
                      e.stopPropagation();
                      setDdl({ conn: c, table });
                    }}
                  >
                    DDL
                  </button>
                </div>
              );
            };
            const renderMissingRow = (table: CatalogTable) => (
              <div
                key={`missing-${tableKey(table)}`}
                className="db-table schema-diff-missing-row ds-object-row"
                title={t("connections.schemaDiffTableMissing")}
              >
                <span className="schema-diff-dot diff-missing" />
                <span className="db-table-ico">{table.kind === "view" ? "◇" : "▦"}</span>
                <span className="tbl-name">{tableLabel(c.engine, table)}</span>
                <span className="schema-diff-inline diff-missing">prod</span>
              </div>
            );
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
                  title={t("connections.migrationsTitle")}
                >
                  <span className="db-nav-ico">◱</span>{" "}
                  {t("connections.migrations")}
                </div>
                {cat && cat.tables.length > 5 && (
                  <input
                    className="table-filter"
                    placeholder={t("connections.filterTables")}
                    value={filter}
                    onChange={(e) =>
                      setFilters((m) => ({ ...m, [c.id]: e.target.value }))
                    }
                  />
                )}
                {cerr && <div className="error small-pad">{cerr}</div>}
                {!cat && !cerr && (
                  <div className="muted small-pad loading">
                    {t("connections.loadingSchema")}
                  </div>
                )}
                {cat && all.length === 0 && missingTables.length === 0 && (
                  <div className="muted small-pad">
                    {f
                      ? t("connections.noTablesMatch", { filter: f })
                      : t("connections.noTables")}
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
                      {t("connections.tables", { count: tbls.length })}
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
                      {t("connections.views", { count: views.length })}
                    </div>
                    {viewsOpen && views.map(renderRow)}
                  </>
                )}
                {missingTables.length > 0 && (
                  <>
                    <div className="db-section schema-diff-section">
                      {t("connections.schemaDiffMissingSection", { count: missingTables.length })}
                    </div>
                    {missingTables.map(renderMissingRow)}
                  </>
                )}
              </div>
            );
          })()}
      </div>
    );
  }

  function renderGroup(group: SchemaConnectionGroup) {
    const isDropTarget =
      dropTarget?.kind === "group" && dropTarget.key === group.key;
    const engine = group.connections[0]?.engine;
    return (
      <div
        key={`group-${group.key}`}
        data-schema-group-key={group.key}
        className={isDropTarget ? "db-group drop-target" : "db-group"}
      >
        <div
          className="db-group-head"
          title={t("connections.schemaGroupTitle", { group: group.label })}
        >
          {engine && <EngineMark engine={engine} />}
          <span className="db-group-name">{group.label}</span>
        </div>
        {group.connections.map((conn) => renderConnection(conn, true))}
      </div>
    );
  }

  return (
    <aside className="sidebar">
      <div className="sidebar-top">
        <div className="sidebar-top-copy" data-tauri-drag-region>
          <span className="sidebar-top-title">{t("connections.sidebarTitle")}</span>
        </div>
        <button
          className="sidebar-add-btn"
          onClick={onNew}
          title={t("connections.new")}
          aria-label={t("connections.new")}
        >
          <Icon name="plus" />
        </button>
      </div>

      <div className="explorer">
        {connections.length === 0 && (
          <div className="muted empty">{t("connections.noConnections")}</div>
        )}
        {sections.map((section) =>
          section.kind === "group"
            ? renderGroup(section.group)
            : renderConnection(section.connection),
        )}
      </div>

      <div className="sidebar-foot">
        <button className="foot-btn" onClick={onOpenSettings}>
          <span className="gear"><Icon name="gear" /></span>{" "}
          {t("common.settings")}
        </button>
      </div>

      {dragPreview &&
        (() => {
          const conn = connectionById(dragPreview.id);
          if (!conn) return null;
          return (
            <div
              className="db-drag-preview"
              style={{
                transform: `translate3d(${Math.round(dragPreview.x + 12)}px, ${Math.round(dragPreview.y + 12)}px, 0)`,
              }}
            >
              <EngineMark engine={conn.engine} />
              <span>{conn.name || t("app.unnamed")}</span>
            </div>
          );
        })()}

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
}: {
  initial: ConnectionProfile | null;
  onSaved: (p: ConnectionProfile) => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
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
      toast(t("connections.connectionSaved"));
      onSaved(saved);
      setMsg(t("connections.saved"));
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
      setMsg(`✓ ${t("connections.connectionOk")}`);
      setMsgErr(false);
    } catch (e) {
      setMsg(errMessage(e));
      setMsgErr(true);
    } finally {
      setBusy(false);
      setRunning(null);
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
      <h2>{isNew ? t("connections.new") : t("connections.edit")}</h2>

      <label>
        {t("connections.name")}
        <input
          value={form.name}
          onChange={(e) => set("name", e.target.value)}
          placeholder="prod-readonly"
        />
      </label>

      <label>
        {t("connections.engine")}
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
          {t("connections.databaseFile")}
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
              {t("connections.browse")}
            </button>
          </div>
        </label>
      ) : (
        <>
          <div className="row">
            <label className="grow">
              {t("connections.host")}
              <input
                value={form.host}
                onChange={(e) => set("host", e.target.value)}
              />
            </label>
            <label className="port">
              {t("connections.port")}
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
            {t("connections.database")}
            <input
              value={form.database}
              onChange={(e) => set("database", e.target.value)}
            />
          </label>

          <div className="row">
            <label className="grow">
              {t("connections.user")}
              <input
                value={form.username}
                onChange={(e) => set("username", e.target.value)}
              />
            </label>
            <label className="grow">
              {t("connections.password")}
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={
                  form.secretRef
                    ? `•••••• (${t("connections.passwordStoredExisting")})`
                    : t("connections.passwordStored")
                }
              />
            </label>
          </div>

          <label>
            {t("connections.sslMode")}
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
        <span className="label-with-help">
          {t("connections.projectFolder")}
          <InfoTip label={t("connections.projectFolderHint")} />
        </span>
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
            {t("connections.browse")}
          </button>
        </div>
      </label>

      <label>
        <span className="label-with-help">
          {t("connections.environment")}
          <InfoTip label={t("connections.environmentHint")} />
        </span>
        <select
          value={form.env ?? ""}
          onChange={(e) => set("env", e.target.value || null)}
        >
          <option value="">{t("common.none")}</option>
          <option value="dev">dev</option>
          <option value="staging">staging</option>
          <option value="prod">prod</option>
        </select>
      </label>

      <label>
        <span className="label-with-help">
          {t("connections.schemaGroup")}
          <InfoTip label={t("connections.schemaGroupHint")} />
        </span>
        <input
          value={form.schemaGroup ?? ""}
          onChange={(e) => set("schemaGroup", e.target.value.trim() || null)}
          placeholder={t("connections.schemaGroupPlaceholder")}
        />
      </label>

      <InfoTip label={t("connections.writeAccessHint")} className="connection-write-help" />

      <div className="form-actions">
        <button className="btn primary" disabled={busy} onClick={save}>
          {running === "save" ? t("common.saving") : t("common.save")}
        </button>
        <button className="btn" disabled={busy} onClick={test}>
          {running === "test" ? t("connections.testing") : t("connections.test")}
        </button>
        <button className="btn" disabled={busy} onClick={onCancel}>
          {t("common.cancel")}
        </button>
      </div>

      {msg && (
        <div className={msgErr ? "form-msg error" : "form-msg ok"}>{msg}</div>
      )}
    </div>
  );
}
