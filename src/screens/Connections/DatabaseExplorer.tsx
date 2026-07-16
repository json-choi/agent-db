// DataGrip-style Database Explorer sidebar: connection tree, DDL modal, schema-group
// drag-and-drop. Split out of the old Connections/index.tsx (see ConnectionForm.tsx
// for the connection create/edit form that used to live alongside it).
import { useEffect, useMemo, useRef, useState, type PointerEvent } from "react";
import { useQueries, useQueryClient } from "@tanstack/react-query";
import {
  deleteConnection,
  getTableDdl,
  setConnectionsSchemaGroup,
} from "../../ipc/commands";
import type { Catalog, CatalogTable, ConnectionProfile } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { catalogQuery, fetchFreshCatalog, qk } from "../../lib/queries";
import {
  buildConnectionSections,
  compareCatalogs,
  defaultSchemaBaseline,
  diffCounts,
  orderTablesBySchemaDiff,
  schemaGroupIsCompatible,
  tableDiffTone,
  type SchemaConnectionGroup,
  type SchemaDiffSummary,
  type TableSchemaDiff,
} from "../../lib/schemaDiff";
import { tableKey, tableLabel } from "../../lib/tableRef";
import ConfirmButton from "../../components/ConfirmButton";
import EngineMark from "../../components/EngineMark";
import { Icon } from "../../components/Icon";
import LazySqlViewer from "../../components/LazySqlViewer";
import { useToast } from "../../components/Toast";
import { useI18n } from "../../lib/i18n";
import "./connections.css";

type DropTarget =
  | { kind: "connection"; id: string }
  | { kind: "group"; key: string };

type DragStart = {
  id: string;
  pointerId: number;
  x: number;
  y: number;
};

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
  activeSchemaGroupKey,
  onSelectConn,
  onOpenTable,
  onOpenSchemaDiff,
  onNew,
  onEdit,
  onDeleted,
  onConnectionUpdated,
  onOpenSettings,
}: {
  connections: ConnectionProfile[];
  selectedId: string | null;
  selectedTableKey: string | null;
  activeSchemaGroupKey: string | null;
  onSelectConn: (id: string) => void;
  onOpenTable: (conn: ConnectionProfile, table: CatalogTable) => void;
  onOpenSchemaDiff: (group: SchemaConnectionGroup) => void;
  onNew: () => void;
  onEdit: (conn: ConnectionProfile) => void;
  onDeleted: (id: string) => void;
  onConnectionUpdated: (conn: ConnectionProfile) => void;
  onOpenSettings: () => void;
}) {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  // Per-connection: any node can be expanded independently of selection, so
  // catalogs/errors/filters are keyed by connection id (DataGrip-style tree).
  // Catalogs come from the shared query cache, so expanding a node here also warms
  // the Schema view and the SQL editor's autocomplete for that connection.
  const [wanted, setWanted] = useState<Set<string>>(new Set());
  const [refreshErrs, setRefreshErrs] = useState<Record<string, string>>({});
  const [filters, setFilters] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [refreshing, setRefreshing] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [tablesOpen, setTablesOpen] = useState(true);
  const [viewsOpen, setViewsOpen] = useState(true);
  const [showRowCounts, setShowRowCounts] = useState(true);
  const [openMenuId, setOpenMenuId] = useState<string | null>(null);
  const [ddl, setDdl] = useState<{ conn: ConnectionProfile; table: CatalogTable } | null>(
    null,
  );
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);
  const [dragPreview, setDragPreview] = useState<{ id: string; x: number; y: number } | null>(
    null,
  );
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

  useEffect(() => {
    if (!openMenuId) return;
    const closeOnOutsidePointer = (event: globalThis.PointerEvent) => {
      const target = event.target;
      if (target instanceof Element && target.closest(".db-menu")) return;
      setOpenMenuId(null);
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    return () => document.removeEventListener("pointerdown", closeOnOutsidePointer);
  }, [openMenuId]);

  const wantedIds = useMemo(() => [...wanted].sort(), [wanted]);
  const { catalogs, loadErrs } = useQueries({
    queries: wantedIds.map((id) => catalogQuery(id)),
    combine: (results) => {
      const catalogs: Record<string, Catalog> = {};
      const loadErrs: Record<string, string> = {};
      results.forEach((result, index) => {
        const id = wantedIds[index];
        if (result.data) catalogs[id] = result.data;
        else if (result.error) loadErrs[id] = errMessage(result.error);
      });
      return { catalogs, loadErrs };
    },
  });
  const errs = { ...loadErrs, ...refreshErrs };

  // Expanding a node subscribes to its catalog; the query cache decides whether that is a
  // fetch or a free read. Retries are not automatic (see the query defaults), so a node
  // that failed refetches when the user expands it again.
  function ensureLoaded(id: string) {
    setWanted((ids) => (ids.has(id) ? ids : new Set(ids).add(id)));
    setRefreshErrs((m) => {
      if (!(id in m)) return m;
      const n = { ...m };
      delete n[id];
      return n;
    });
    if (queryClient.getQueryState(qk.catalog(id))?.status === "error") {
      void queryClient.refetchQueries({ queryKey: qk.catalog(id) });
    }
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
  // Writing the result into the shared cache updates every surface reading this catalog.
  async function refreshSchema(id: string) {
    setRefreshing(id);
    setRefreshErrs((m) => {
      const n = { ...m };
      delete n[id];
      return n;
    });
    try {
      queryClient.setQueryData(qk.catalog(id), await fetchFreshCatalog(id));
      setWanted((ids) => (ids.has(id) ? ids : new Set(ids).add(id)));
    } catch (e) {
      setRefreshErrs((m) => ({ ...m, [id]: errMessage(e) }));
    } finally {
      setRefreshing(null);
    }
  }

  async function removeConnection(conn: ConnectionProfile) {
    setDeleting(conn.id);
    try {
      await deleteConnection(conn.id);
      setWanted((ids) => {
        if (!ids.has(conn.id)) return ids;
        const next = new Set(ids);
        next.delete(conn.id);
        return next;
      });
      queryClient.removeQueries({ queryKey: qk.catalog(conn.id) });
      toast(t("connections.connectionDeleted"));
      onDeleted(conn.id);
    } catch (e) {
      toast(errMessage(e), "error");
    } finally {
      setDeleting(null);
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

  // Schema group comparison is SQL-only, so MongoDB connections can neither be dragged
  // into a group nor accept one dropped on them — same-engine already implies "both or
  // neither" once one side is excluded.
  function canDropOnConnection(dragId: string | null, target: ConnectionProfile) {
    const dragged = dragId ? connectionById(dragId) : null;
    return (
      !!dragged &&
      dragged.id !== target.id &&
      dragged.engine === target.engine &&
      dragged.engine !== "mongodb"
    );
  }

  function canDropOnGroup(dragId: string | null, group: SchemaConnectionGroup) {
    const dragged = dragId ? connectionById(dragId) : null;
    const engine = group.connections[0]?.engine;
    return (
      !!dragged &&
      !!engine &&
      dragged.engine === engine &&
      dragged.engine !== "mongodb" &&
      !group.connections.some((conn) => conn.id === dragged.id)
    );
  }

  async function saveSchemaGroupUpdates(ids: string[], schemaGroup: string) {
    const originals = ids
      .map((id) => connectionById(id))
      .filter((conn): conn is ConnectionProfile => !!conn);
    for (const id of ids) {
      const original = connectionById(id);
      if (original) onConnectionUpdated({ ...original, schemaGroup });
    }
    try {
      const saved = await setConnectionsSchemaGroup(ids, schemaGroup);
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
    const ids = [
      ...(draggedGroup === group ? [] : [dragged.id]),
      ...(targetGroup === group ? [] : [target.id]),
    ];
    if (ids.length === 0) return;
    const confirmed = window.confirm(
      t("connections.schemaGroupConfirmPair", {
        source: dragged.name || dragged.database || t("app.unnamed"),
        target: target.name || target.database || t("app.unnamed"),
        group,
      }),
    );
    if (!confirmed) return;
    try {
      await saveSchemaGroupUpdates(ids, group);
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
      await saveSchemaGroupUpdates([dragged.id], group.label);
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

  function groupBaseline(group: SchemaConnectionGroup): ConnectionProfile | null {
    return defaultSchemaBaseline(group);
  }

  function schemaDiffForConnection(conn: ConnectionProfile): SchemaDiffSummary | null {
    const group = groupByConnectionId.get(conn.id);
    if (!group) return null;
    const baseline = groupBaseline(group);
    if (!baseline || baseline.id === conn.id) return null;
    const current = catalogs[conn.id];
    const baselineCatalog = catalogs[baseline.id];
    if (!current || !baselineCatalog) return null;
    return compareCatalogs(current, baselineCatalog);
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
    const baseline = groupBaseline(group);
    if (!baseline || baseline.id === conn.id) return null;
    const current = catalogs[conn.id];
    const baselineCatalog = catalogs[baseline.id];
    if (!current || !baselineCatalog) {
      return (
        <span className="schema-diff-chip diff-pending" title={t("connections.schemaDiffPendingTitle")}>
          {t("connections.schemaDiffPendingChip")}
        </span>
      );
    }
    const diff = compareCatalogs(current, baselineCatalog);
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
          <div
            className={`db-menu${openMenuId === c.id ? " open" : ""}`}
            onPointerDown={(e) => e.stopPropagation()}
            onPointerUp={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Escape") {
                e.preventDefault();
                setOpenMenuId(null);
                e.currentTarget.querySelector<HTMLButtonElement>(".db-menu-trigger")?.focus();
              }
            }}
          >
            <button
              type="button"
              className="db-menu-trigger"
              title={t("connections.connectionMenu")}
              aria-label={t("connections.connectionMenu")}
              aria-expanded={openMenuId === c.id}
              aria-controls={`connection-menu-${c.id}`}
              onClick={() => setOpenMenuId((current) => (current === c.id ? null : c.id))}
            >
              <Icon name="moreVertical" />
            </button>
            {openMenuId === c.id && (
              <div className="db-menu-panel" id={`connection-menu-${c.id}`}>
                <button
                  type="button"
                  onClick={() => {
                    setOpenMenuId(null);
                    onEdit(c);
                  }}
                >
                  {t("connections.edit")}
                </button>
                <button
                  type="button"
                  disabled={refreshing === c.id}
                  onClick={() => {
                    setOpenMenuId(null);
                    void refreshSchema(c.id);
                  }}
                >
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
                  disabled={deleting === c.id}
                  onConfirm={() => void removeConnection(c)}
                >
                  {t("common.delete")}
                </ConfirmButton>
              </div>
            )}
          </div>
        </div>

        {open.has(c.id) &&
          (() => {
            const cat = catalogs[c.id];
            const cerr = errs[c.id];
            const diff = schemaDiffForConnection(c);
            const filter = filters[c.id] ?? "";
            const f = filter.trim().toLowerCase();
            const filteredTables = cat
              ? f
                ? cat.tables.filter((t) => tableMatchesFilter(t, f))
                : cat.tables
              : [];
            const all = orderTablesBySchemaDiff(filteredTables, diff);
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
                  <span
                    className={`schema-diff-dot${tone ? ` diff-${tone}` : " diff-none"}`}
                    title={tableDiff ? tableDiffTitle(tableDiff) : undefined}
                    aria-hidden="true"
                  />
                  <span className="tbl-name">
                    {tableLabel(c.engine, table)}
                  </span>
                  {showRowCounts && table.rowEstimate != null && table.rowEstimate >= 0 && (
                    <span className="tbl-count muted">
                      ~{table.rowEstimate.toLocaleString()}
                    </span>
                  )}
                  {/* CREATE-TABLE DDL is a SQL-only concept; MongoDB collections have none. */}
                  {c.engine !== "mongodb" && (
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
                  )}
                </div>
              );
            };
            const renderMissingRow = (table: CatalogTable) => (
              <div
                key={`missing-${tableKey(table)}`}
                className="db-table schema-diff-missing-row ds-object-row"
                title={t("connections.schemaDiffTableMissing")}
              >
                <span className="schema-diff-dot diff-missing" aria-hidden="true" />
                <span className="tbl-name">{tableLabel(c.engine, table)}</span>
                <span className="schema-diff-kind">
                  {t(table.kind === "view" ? "schemaDiff.objectView" : "schemaDiff.objectTable")}
                </span>
                <span className="schema-diff-inline diff-missing">base</span>
              </div>
            );
            return (
              <div className="db-tables">
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
    const baseline = defaultSchemaBaseline(group);
    const baselineCatalog = baseline ? catalogs[baseline.id] : undefined;
    const targets = group.connections.filter((connection) => connection.id !== baseline?.id);
    const diffs = baselineCatalog
      ? targets.flatMap((connection) => {
          const catalog = catalogs[connection.id];
          return catalog ? [compareCatalogs(catalog, baselineCatalog)] : [];
        })
      : [];
    const complete = targets.length > 0 && diffs.length === targets.length;
    const groupCounts = diffs.reduce(
      (total, diff) => {
        const counts = diffCounts(diff);
        total.added += counts.added;
        total.missing += counts.missing;
        total.changed += counts.changed;
        return total;
      },
      { added: 0, missing: 0, changed: 0 },
    );
    const groupTotal = groupCounts.added + groupCounts.missing + groupCounts.changed;
    return (
      <div
        key={`group-${group.key}`}
        data-schema-group-key={group.key}
        className={isDropTarget ? "db-group drop-target" : "db-group"}
      >
        <div
          className={`db-group-head${activeSchemaGroupKey === group.key ? " active" : ""}`}
          title={t("connections.schemaGroupTitle", { group: group.label })}
        >
          {engine && <EngineMark engine={engine} />}
          <span className="db-group-name">{group.label}</span>
          <button
            type="button"
            className="db-group-compare"
            title={t("schemaDiff.openTitle")}
            aria-label={t("schemaDiff.openTitle")}
            onClick={() => {
              for (const connection of group.connections) ensureLoaded(connection.id);
              onOpenSchemaDiff(group);
            }}
          >
            {!schemaGroupIsCompatible(group) ? (
              <Icon name="alert" />
            ) : complete && groupTotal === 0 ? (
              <Icon name="check" />
            ) : complete ? (
              <span className="db-group-diff-counts">
                {groupCounts.added > 0 && <span className="diff-add">+{groupCounts.added}</span>}
                {groupCounts.missing > 0 && <span className="diff-remove">−{groupCounts.missing}</span>}
                {groupCounts.changed > 0 && <span className="diff-change">~{groupCounts.changed}</span>}
              </span>
            ) : (
              <span>{t("schemaDiff.open")}</span>
            )}
          </button>
        </div>
        {group.connections.map((conn) => renderConnection(conn, true))}
      </div>
    );
  }

  return (
    <aside className="sidebar">
      <div className="sidebar-top" data-tauri-drag-region="deep">
        <div className="sidebar-top-copy">
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
