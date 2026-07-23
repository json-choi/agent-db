import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useQuery } from "@tanstack/react-query";
import { getSafety, listConnections } from "./ipc/commands";
import type {
  CatalogTable,
  ConnectionProfile,
  Dashboard,
  SafetySettings,
} from "./ipc/types";
import { errMessage } from "./ipc/types";
import { hasCapability, isDocumentEngine } from "./lib/capabilities";
import {
  driversQuery,
  isTransientDbError,
  mcpPlatformsQuery,
  mcpRuntimeStatusQuery,
} from "./lib/queries";
import { buildConnectionSections, type SchemaConnectionGroup } from "./lib/schemaDiff";
import { tableKey, tableLabel } from "./lib/tableRef";
import { ConnectionForm, DatabaseExplorer } from "./screens/Connections";
import TableData from "./screens/Tables";
import SchemaExplorer from "./screens/Schema";
import Sql from "./screens/Sql";
import Documents from "./screens/Documents";
import Dashboards, { DashboardSidebar } from "./screens/Dashboards";
import Activity from "./screens/Activity";
import SchemaDiff from "./screens/SchemaDiff";
import Onboarding from "./screens/Onboarding";
import Settings from "./screens/Settings";
import AgentChat from "./screens/AgentChat";
import AgentLogDialog from "./components/AgentLogDialog";
import EngineMark from "./components/EngineMark";
import { Icon } from "./components/Icon";
import WorkbenchDocumentStrip from "./components/WorkbenchDocumentStrip";
import { ToastProvider, useToast } from "./components/Toast";
import WorkspaceAccount from "./components/WorkspaceAccount";
import WorkspaceSwitcher from "./components/WorkspaceSwitcher";
import { AgentChatProvider } from "./lib/agentChat";
import { AgentFeedProvider, useAgentFeed } from "./lib/agentFeed";
import { useI18n, type I18nKey } from "./lib/i18n";
import {
  queryDocument,
  stableDocument,
  supportsDocument,
  tableDocument,
  type WorkbenchDocument,
} from "./lib/workbenchDocuments";

// Chat2DB-style information architecture:
// - the global rail switches products (database workspace / dashboard);
// - database tools are real documents inside the selected connection's workbench;
// - Agent remains a persistent utility dock and inherits that one connection context.
type AppArea = "workspace" | "dashboard";

// `null` = not editing; "new" = blank form; a profile = edit that profile.
type Editing = ConnectionProfile | "new" | null;
type McpBadgeState = "server" | "disconnected";

export default function App() {
  return (
    <ToastProvider>
      <AgentFeedProvider>
        <AgentChatProvider>
          <Shell />
        </AgentChatProvider>
      </AgentFeedProvider>
    </ToastProvider>
  );
}

const SIDEBAR_MIN = 180;
const IS_MACOS = typeof navigator !== "undefined"
  && /Macintosh|Mac OS X/.test(navigator.userAgent);
const SIDEBAR_MAX = 520;
const SIDEBAR_DEFAULT = 240;
const MCP_PLATFORM_REFRESH_INTERVAL_MS = 5 * 60_000;
const MCP_RUNTIME_REFRESH_INTERVAL_MS = 30_000;
const MCP_STARTUP_POLL_INTERVAL_MS = 2_000;
const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;

function preloadSqlEditor() {
  void import("./components/SqlViewer").catch(() => undefined);
}

function connectionEndpoint(conn: ConnectionProfile) {
  if (conn.engine === "sqlite") return conn.database || conn.host || "sqlite";
  return `${conn.host}${conn.port ? `:${conn.port}` : ""}`;
}

function ConnectionPicker({
  connections,
  onSelect,
  onNew,
}: {
  connections: ConnectionProfile[];
  onSelect: (id: string) => void;
  onNew: () => void;
}) {
  const { t } = useI18n();
  const sections = useMemo(() => buildConnectionSections(connections), [connections]);
  const grouped = sections.filter((section) => section.kind === "group");
  const singles = sections.filter((section) => section.kind === "single");

  function renderConnectionCard(conn: ConnectionProfile, grouped = false) {
    const name = conn.name || t("app.unnamed");
    return (
      <button
        key={conn.id}
        type="button"
        className="connection-card"
        onClick={() => onSelect(conn.id)}
        title={`${conn.engine} · ${connectionEndpoint(conn)} · ${conn.database}`}
        aria-label={t("app.openConnection", { name })}
      >
        <span className="connection-card-title">
          {!grouped && <EngineMark engine={conn.engine} />}
          <span className="connection-card-name">{name}</span>
          {conn.env && <span className={`env-chip env-${conn.env}`}>{conn.env}</span>}
        </span>
        <span className="connection-card-meta">
          <span>{conn.database || t("common.unknown")}</span>
          <span className="ds-meta-dot" />
          <span>{connectionEndpoint(conn)}</span>
        </span>
      </button>
    );
  }

  function renderGroup(group: SchemaConnectionGroup) {
    const engine = group.connections[0]?.engine;
    return (
      <section className="connection-group-section" key={group.key}>
        <div className="connection-group-head">
          <div className="connection-group-title">
            {engine ? <EngineMark engine={engine} /> : <span className="connection-group-mark" />}
            <span>{group.label}</span>
          </div>
        </div>
        <div className="connection-card-grid">
          {group.connections.map((conn) => renderConnectionCard(conn, true))}
        </div>
      </section>
    );
  }

  return (
    <div className="connection-picker">
      <div className="connection-picker-head">
        <h2>{t("app.connectionPickerTitle")}</h2>
        <button className="btn small" onClick={onNew}>
          <Icon name="plus" />
          {t("connections.new")}
        </button>
      </div>

      {grouped.length > 0 && (
        <section className="connection-picker-section">
          <div className="connection-picker-label">{t("app.connectionPickerGroups")}</div>
          {grouped.map((section) =>
            section.kind === "group" ? renderGroup(section.group) : null,
          )}
        </section>
      )}

      {singles.length > 0 && (
        <section className="connection-picker-section">
          <div className="connection-picker-label">{t("app.connectionPickerSingles")}</div>
          <div className="connection-card-grid">
            {singles.map((section) =>
              section.kind === "single" ? renderConnectionCard(section.connection) : null,
            )}
          </div>
        </section>
      )}
    </div>
  );
}

// Product-level rail. Data/ER/query/history are intentionally absent: those actions
// live in the workbench document bar, so there is only one path to each tool.
function WorkbenchRail({
  area,
  dashboardAvailable,
  settingsOpen,
  account,
  onArea,
  onSettings,
}: {
  area: AppArea | null;
  dashboardAvailable: boolean;
  settingsOpen: boolean;
  account: ReactNode;
  onArea: (area: AppArea) => void;
  onSettings: () => void;
}) {
  const { t } = useI18n();
  const items: Array<{
    id: AppArea;
    icon: "database" | "dashboard";
    label: I18nKey;
  }> = [
    { id: "workspace", icon: "database", label: "workspace.label" },
    { id: "dashboard", icon: "dashboard", label: "tabs.dashboard" },
  ];
  return (
    <nav
      className="workbench-rail"
      aria-label={t("app.workbenchNavigation")}
      onKeyDown={(event) => {
        if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(event.key)) {
          return;
        }
        const buttons = [
          ...event.currentTarget.querySelectorAll<HTMLButtonElement>(
            ".workbench-rail-button:not(:disabled), [data-rail-control]:not(:disabled)",
          ),
        ];
        const current = buttons.indexOf(event.target as HTMLButtonElement);
        if (current < 0) return;
        event.preventDefault();
        const direction =
          event.key === "ArrowDown" || event.key === "ArrowRight" ? 1 : -1;
        buttons[(current + direction + buttons.length) % buttons.length]?.focus();
      }}
    >
      {/* Overlay title bars place native traffic lights above the webview. This
          structural slot reserves that OS-owned rectangle before any app control. */}
      <div
        className="workbench-window-controls-safe"
        data-window-controls-safe-zone
        data-tauri-drag-region="deep"
        aria-hidden="true"
      />
      <div className="workbench-rail-brand">d</div>
      <div className="workbench-rail-items">
        {items.map((item) => (
          <button
            key={item.id}
            type="button"
            className={`workbench-rail-button${area === item.id ? " active" : ""}`}
            onClick={() => onArea(item.id)}
            title={t(item.label)}
            aria-label={t(item.label)}
            aria-current={area === item.id ? "page" : undefined}
            disabled={item.id === "dashboard" && !dashboardAvailable}
          >
            <Icon name={item.icon} />
          </button>
        ))}
      </div>
      <div className="workbench-rail-bottom">
        {account}
        <button
          type="button"
          className={`workbench-rail-button${settingsOpen ? " active" : ""}`}
          onClick={onSettings}
          title={t("common.settings")}
          aria-label={t("common.settings")}
          aria-current={settingsOpen ? "page" : undefined}
        >
          <Icon name="gear" />
        </button>
      </div>
    </nav>
  );
}

function Shell() {
  const { t } = useI18n();
  const { unseen, latest, markSeen } = useAgentFeed();
  const toast = useToast();
  const [conns, setConns] = useState<ConnectionProfile[]>([]);
  // Resizable sidebar: drag the divider, double-click resets; width persists.
  const [sidebarW, setSidebarW] = useState(() => {
    const w = Number(localStorage.getItem("sidebarW"));
    return w >= SIDEBAR_MIN && w <= SIDEBAR_MAX ? w : SIDEBAR_DEFAULT;
  });
  const startSidebarDrag = (e: { preventDefault(): void; clientX: number }) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = sidebarW;
    const clamp = (w: number) => Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, w));
    const move = (ev: MouseEvent) => setSidebarW(clamp(startW + ev.clientX - startX));
    const up = (ev: MouseEvent) => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
      localStorage.setItem("sidebarW", String(clamp(startW + ev.clientX - startX)));
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  };
  const [selectedId, setSelectedId] = useState<string | null>(() =>
    localStorage.getItem("selectedId"),
  );
  const [editing, setEditing] = useState<Editing>(null);
  const [safety, setSafety] = useState<SafetySettings | null>(null);
  const [safetyError, setSafetyError] = useState<string | null>(null);
  // A user who last had the old Audit tab open should land in its expanded details
  // after the two top-level tabs are consolidated into Activity.
  const legacyAuditOpen = useRef(localStorage.getItem("tab") === "audit");
  const restoredDocumentKind = useRef<WorkbenchDocument["kind"]>(
    (() => {
      const saved = localStorage.getItem("tab");
      if (saved === "history" || saved === "audit") return "activity";
      if (saved === "sql" || saved === "documents" || saved === "schema") return saved;
      return "schema";
    })(),
  ).current;
  const [area, setArea] = useState<AppArea>(() =>
    localStorage.getItem("appArea") === "dashboard" || localStorage.getItem("tab") === "dashboard"
      ? "dashboard"
      : "workspace",
  );
  const [documents, setDocuments] = useState<WorkbenchDocument[]>([]);
  const [activeDocumentId, setActiveDocumentId] = useState<string | null>(null);
  const [agentDockOpen, setAgentDockOpen] = useState(
    () => localStorage.getItem("agentDockOpen") !== "0",
  );
  const [agentOverlay, setAgentOverlay] = useState(
    () => window.matchMedia("(max-width: 900px)").matches,
  );
  const [dashboardFocusId, setDashboardFocusId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<
    "mcp" | "safety" | "updates" | undefined
  >(undefined);
  const [schemaDiffGroupKey, setSchemaDiffGroupKey] = useState<string | null>(null);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const [agentLogOpen, setAgentLogOpen] = useState(false);
  const [agentHistoryOpen, setAgentHistoryOpen] = useState(false);
  const agentButtonRef = useRef<HTMLButtonElement | null>(null);
  const agentCloseRef = useRef<HTMLButtonElement | null>(null);
  const agentHistoryButtonRef = useRef<HTMLButtonElement | null>(null);
  const agentHistoryOpenRef = useRef(false);
  const agentDockRef = useRef<HTMLElement | null>(null);
  const updateCheckInFlight = useRef(false);
  const lastUpdateCheckAt = useRef(0);
  const mcpPlatformsQ = useQuery({
    ...mcpPlatformsQuery(),
    refetchInterval: MCP_PLATFORM_REFRESH_INTERVAL_MS,
  });
  const mcpRuntimeQ = useQuery({
    ...mcpRuntimeStatusQuery(),
    refetchInterval: (query) =>
      query.state.data?.httpRunning
        ? MCP_RUNTIME_REFRESH_INTERVAL_MS
        : MCP_STARTUP_POLL_INTERVAL_MS,
  });
  const mcpBadgeReady = !mcpPlatformsQ.isPending && !mcpRuntimeQ.isPending;
  const mcpBadge: McpBadgeState | null = !mcpBadgeReady
    ? null
    : mcpRuntimeQ.isError ||
        !mcpRuntimeQ.data?.httpRunning ||
        !mcpRuntimeQ.data.bridgeRunning
      ? "server"
      : mcpPlatformsQ.isError || !mcpPlatformsQ.data?.some((platform) => platform.connected)
        ? "disconnected"
        : null;

  const openDashboard = useCallback((dashboard: Dashboard) => {
    setSelectedId(dashboard.connectionId);
    setEditing(null);
    setSettingsOpen(false);
    setSchemaDiffGroupKey(null);
    setDashboardFocusId(dashboard.id);
    setArea("dashboard");
  }, []);
  const consumeDashboardFocus = useCallback(() => setDashboardFocusId(null), []);

  // Both the local save command and MCP create_dashboard emit the same direct
  // Dashboard payload. This makes an explicitly requested dashboard visible at once.
  useEffect(() => {
    const pending = listen<Dashboard>("dashboard:created", (event) => {
      openDashboard(event.payload);
    }).catch((error) => console.error("dashboard event listen failed:", error));
    return () => {
      void pending.then((unlisten) => unlisten && unlisten());
    };
  }, [openDashboard]);

  // Persist the product area and selected connection. Keep writing the legacy `tab`
  // key for one release so older builds can still restore a sensible destination.
  useEffect(() => {
    localStorage.setItem("appArea", area);
    localStorage.setItem("tab", area === "dashboard" ? "dashboard" : "data");
  }, [area]);
  useEffect(() => {
    if (selectedId) localStorage.setItem("selectedId", selectedId);
    else localStorage.removeItem("selectedId");
  }, [selectedId]);

  const selected = conns.find((c) => c.id === selectedId) ?? null;
  const selectedDocuments = useMemo(
    () => documents.filter((document) => document.connectionId === selectedId),
    [documents, selectedId],
  );
  const activeDocument =
    selectedDocuments.find((document) => document.id === activeDocumentId) ?? null;
  const selectedTable = activeDocument?.kind === "data" ? activeDocument.table : null;
  const showAgentDock = agentDockOpen && !!selected && !settingsOpen && editing === null;
  // Schema diff is a SQL-only comparison feature — a group whose connections are MongoDB
  // is never a valid diff candidate, even if one somehow carries a schemaGroup value.
  const schemaGroups = useMemo(
    () =>
      buildConnectionSections(conns).flatMap((section) =>
        section.kind === "group" && !isDocumentEngine(section.group.connections[0]?.engine)
          ? [section.group]
          : [],
      ),
    [conns],
  );
  const activeSchemaGroup =
    schemaGroups.find((group) => group.key === schemaDiffGroupKey) ?? null;

  // SQL and Documents are mutually exclusive per connection, gated by the resolved
  // driver capability. Engine fallback avoids a SQL/Documents flash while drivers load.
  const driversQ = useQuery(driversQuery());
  const supportsSql =
    !selected ||
    (driversQ.data
      ? hasCapability(driversQ.data, selected, "sql")
      : !isDocumentEngine(selected.engine));
  const initializedConnection = useRef<string | null>(null);
  useEffect(() => {
    if (!selected) {
      if (initializedConnection.current !== null || documents.length > 0) {
        initializedConnection.current = null;
        setDocuments([]);
      }
      if (activeDocumentId !== null) setActiveDocumentId(null);
      return;
    }
    if (initializedConnection.current === selected.id) {
      const valid = documents.filter(
        (document) => supportsDocument(document, selected.id, supportsSql),
      );
      if (valid.length !== documents.length) {
        setDocuments(valid);
        setActiveDocumentId((current) =>
          valid.some((document) => document.id === current) ? current : (valid[0]?.id ?? null),
        );
      }
      return;
    }

    initializedConnection.current = selected.id;
    const preferred = supportsSql
      ? restoredDocumentKind === "documents"
        ? "schema"
        : restoredDocumentKind
      : restoredDocumentKind === "sql"
        ? "documents"
        : restoredDocumentKind;
    const initial =
      preferred === "sql" || preferred === "documents"
        ? queryDocument(selected.id, preferred)
        : stableDocument(
            selected.id,
            preferred === "activity" ? "activity" : "schema",
          );
    setDocuments([initial]);
    setActiveDocumentId(initial.id);
  }, [activeDocumentId, documents, restoredDocumentKind, selected, supportsSql]);

  useEffect(() => {
    const media = window.matchMedia("(max-width: 900px)");
    const sync = () => setAgentOverlay(media.matches);
    media.addEventListener("change", sync);
    return () => media.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    agentHistoryOpenRef.current = agentHistoryOpen;
  }, [agentHistoryOpen]);

  useEffect(() => {
    if (!showAgentDock || !agentOverlay) return;
    const close = () => {
      localStorage.setItem("agentDockOpen", "0");
      setAgentDockOpen(false);
      setAgentHistoryOpen(false);
      window.requestAnimationFrame(() => agentButtonRef.current?.focus());
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        if (agentHistoryOpenRef.current) {
          setAgentHistoryOpen(false);
          window.requestAnimationFrame(() => agentHistoryButtonRef.current?.focus());
          return;
        }
        close();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = [
        ...(agentDockRef.current?.querySelectorAll<HTMLElement>(
          'button:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])',
        ) ?? []),
      ];
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (
        event.shiftKey &&
        (document.activeElement === first ||
          !agentDockRef.current?.contains(document.activeElement))
      ) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    const inertTargets = [
      document.querySelector<HTMLElement>(".main"),
      document.querySelector<HTMLElement>(".sidebar"),
    ].filter((target): target is HTMLElement => target !== null);
    inertTargets.forEach((target) => target.setAttribute("inert", ""));
    const focusFrame = window.requestAnimationFrame(() => agentCloseRef.current?.focus());
    document.addEventListener("keydown", onKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      document.removeEventListener("keydown", onKeyDown);
      inertTargets.forEach((target) => target.removeAttribute("inert"));
    };
  }, [agentOverlay, showAgentDock]);

  // CodeMirror is intentionally split out of the startup bundle. Warm that chunk only
  // after a SQL-capable connection exists, using idle time so the first SQL click does
  // not also pay module download/parse cost.
  useEffect(() => {
    if (!selected || !supportsSql) return;
    if (typeof window.requestIdleCallback === "function") {
      const id = window.requestIdleCallback(preloadSqlEditor, { timeout: 1_500 });
      return () => window.cancelIdleCallback(id);
    }
    const id = window.setTimeout(preloadSqlEditor, 300);
    return () => window.clearTimeout(id);
  }, [selected?.id, supportsSql]);

  useEffect(() => {
    if (schemaDiffGroupKey && !activeSchemaGroup) setSchemaDiffGroupKey(null);
  }, [activeSchemaGroup, schemaDiffGroupKey]);

  // Transient (network-shaped) load failures retry themselves with backoff instead of
  // parking on the error card until the user clicks retry; deterministic failures still
  // surface immediately. Manual retry re-enters at attempt 0.
  function refresh(attempt = 0): Promise<ConnectionProfile[]> {
    return listConnections()
      .then((cs) => {
        setConns(cs);
        setLoadError(null);
        return cs;
      })
      .catch((e) => {
        if (attempt < 3 && isTransientDbError(e)) {
          return new Promise<void>((resolve) =>
            window.setTimeout(resolve, Math.min(1000 * 2 ** attempt, 8_000)),
          ).then(() => refresh(attempt + 1));
        }
        setLoadError(errMessage(e));
        return [];
      });
  }

  async function reloadWorkspaceScope() {
    setSelectedId(null);
    initializedConnection.current = null;
    setDocuments([]);
    setActiveDocumentId(null);
    setEditing(null);
    setSettingsOpen(false);
    setSchemaDiffGroupKey(null);
    setDashboardFocusId(null);
    setSafety(null);
    setConns([]);
    await refresh();
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function refreshAvailableUpdate() {
    if (updateCheckInFlight.current) return;
    updateCheckInFlight.current = true;
    try {
      const next = await check();
      lastUpdateCheckAt.current = Date.now();
      setAvailableUpdate(next);
    } catch {
      lastUpdateCheckAt.current = Date.now();
    } finally {
      updateCheckInFlight.current = false;
    }
  }

  useEffect(() => {
    void refreshAvailableUpdate();
    const iv = window.setInterval(() => void refreshAvailableUpdate(), UPDATE_CHECK_INTERVAL_MS);
    const onVisibility = () => {
      if (document.hidden) return;
      if (Date.now() - lastUpdateCheckAt.current >= UPDATE_CHECK_INTERVAL_MS) {
        void refreshAvailableUpdate();
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.clearInterval(iv);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  // Nudge (not hijack) when a new result lands while the Agent dock is hidden. Skip
  // the mount baseline so it doesn't fire on load; guard on result id so it fires once per
  // result; throttle to one toast per 30s so a burst of agent queries yields a single nudge.
  const seenResultId = useRef<number | null>(null);
  const surfaceInit = useRef(true);
  const lastToastAt = useRef(0);
  useEffect(() => {
    if (surfaceInit.current) {
      surfaceInit.current = false;
      seenResultId.current = latest?.id ?? null;
      return;
    }
    if (latest && latest.id !== seenResultId.current) {
      seenResultId.current = latest.id;
      const now = Date.now();
      if (!showAgentDock && now - lastToastAt.current > 30000) {
        lastToastAt.current = now;
        toast(t("app.toastAgentQuery"));
      }
    }
  }, [latest, showAgentDock, toast]);

  // Agent is now the conversation itself; activity is only considered seen after the user
  // explicitly opens the secondary log surface.
  useEffect(() => {
    if (agentLogOpen && unseen > 0) markSeen();
  }, [agentLogOpen, unseen, markSeen]);

  // Per-connection safety drives the Data/SQL views (max rows, auto-run reads).
  // safetyReqId guards against out-of-order resolution: getSafety runs on a pooled
  // SqlitePool so a fast A→B connection switch can resolve A last — only apply a
  // response if its id is still the latest requested one.
  const safetyReqId = useRef<string | null>(null);
  function loadSafety(id: string) {
    safetyReqId.current = id;
    setSafety(null);
    setSafetyError(null);
    getSafety(id)
      .then((s) => {
        if (safetyReqId.current === id) setSafety(s);
      })
      .catch((e) => {
        if (safetyReqId.current === id) setSafetyError(errMessage(e));
      });
  }

  useEffect(() => {
    if (selectedId) loadSafety(selectedId);
    else {
      safetyReqId.current = null;
      setSafety(null);
    }
  }, [selectedId]);

  const refreshSafety = () => {
    if (selectedId) loadSafety(selectedId);
  };

  function loadSql(sql: string) {
    if (!selected) return;
    const document = queryDocument(
      selected.id,
      supportsSql ? "sql" : "documents",
      sql,
    );
    setDocuments((current) => [...current, document]);
    setActiveDocumentId(document.id);
    setArea("workspace");
  }

  function openMcpSettings() {
    setSettingsSection("mcp");
    setSettingsOpen(true);
    setSchemaDiffGroupKey(null);
    setEditing(null);
  }

  function openUpdateSettings() {
    setSettingsSection("updates");
    setSettingsOpen(true);
    setSchemaDiffGroupKey(null);
    setEditing(null);
  }

  function openAgentDock() {
    localStorage.setItem("agentDockOpen", "1");
    setAgentDockOpen(true);
  }

  function closeAgentDock() {
    localStorage.setItem("agentDockOpen", "0");
    setAgentDockOpen(false);
    setAgentHistoryOpen(false);
  }

  function toggleAgentDock() {
    if (!selected) return;
    if (showAgentDock) {
      closeAgentDock();
      return;
    }
    setSettingsOpen(false);
    setEditing(null);
    setSchemaDiffGroupKey(null);
    openAgentDock();
  }

  function syncAvailableUpdate(update: Update | null) {
    lastUpdateCheckAt.current = Date.now();
    setAvailableUpdate(update);
  }

  function selectConnection(id: string, nextArea: AppArea = area) {
    const connection = conns.find((candidate) => candidate.id === id);
    const initial = connection && isDocumentEngine(connection.engine)
      ? queryDocument(id, "documents")
      : stableDocument(id, "schema");
    initializedConnection.current = id;
    setSelectedId(id);
    setDocuments([initial]);
    setActiveDocumentId(initial.id);
    setEditing(null);
    setSettingsOpen(false);
    setSchemaDiffGroupKey(null);
    setDashboardFocusId(null);
    setAgentLogOpen(false);
    setArea(nextArea);
  }

  function activateDocument(document: WorkbenchDocument) {
    setDocuments((current) =>
      current.some((candidate) => candidate.id === document.id)
        ? current
        : [...current, document],
    );
    setActiveDocumentId(document.id);
    setEditing(null);
    setSettingsOpen(false);
    setSchemaDiffGroupKey(null);
    setArea("workspace");
  }

  function openTableDocument(connection: ConnectionProfile, table: CatalogTable) {
    if (selectedId !== connection.id) {
      initializedConnection.current = connection.id;
      setSelectedId(connection.id);
      setDocuments([]);
    }
    activateDocument(tableDocument(connection.id, table));
  }

  function openStableDocument(kind: "schema" | "activity") {
    if (!selected) return;
    activateDocument(stableDocument(selected.id, kind));
  }

  function openQueryDocument() {
    if (!selected) return;
    preloadSqlEditor();
    activateDocument(queryDocument(selected.id, supportsSql ? "sql" : "documents"));
  }

  function closeDocument(id: string) {
    const index = selectedDocuments.findIndex((document) => document.id === id);
    if (index < 0) return;
    let remaining = selectedDocuments.filter((document) => document.id !== id);
    if (remaining.length === 0 && selected && supportsSql) {
      remaining = [stableDocument(selected.id, "schema")];
    }
    setDocuments((current) => [
      ...current.filter((document) => document.connectionId !== selectedId),
      ...remaining,
    ]);
    if (activeDocumentId === id) {
      setActiveDocumentId(
        remaining[Math.min(index, Math.max(0, remaining.length - 1))]?.id ?? null,
      );
    }
  }

  function setActiveQueryDraft(value: string) {
    if (!activeDocument || (activeDocument.kind !== "sql" && activeDocument.kind !== "documents")) {
      return;
    }
    setDocuments((current) =>
      current.map((document) =>
        document.id === activeDocument.id ? { ...document, draft: value } : document,
      ),
    );
  }

  function startNewConnection() {
    setEditing("new");
    setSettingsOpen(false);
    setSchemaDiffGroupKey(null);
  }

  function renderMain() {
    if (settingsOpen) {
      return (
        <Settings
          connection={selected}
          initialSection={settingsSection}
          refreshSafety={refreshSafety}
          availableUpdate={availableUpdate}
          onUpdateChecked={syncAvailableUpdate}
          onClose={() => setSettingsOpen(false)}
        />
      );
    }
    if (activeSchemaGroup) {
      return (
        <SchemaDiff
          key={activeSchemaGroup.key}
          group={activeSchemaGroup}
          onClose={() => setSchemaDiffGroupKey(null)}
        />
      );
    }
    if (editing !== null) {
      return (
        <div className="editor-pane">
          <ConnectionForm
            initial={editing === "new" ? null : editing}
            onSaved={async (p) => {
              await refresh();
              setSelectedId(p.id);
              setEditing(null);
            }}
            onCancel={() => setEditing(null)}
          />
        </div>
      );
    }
    // Startup failure takes precedence over onboarding: a store read error must not
    // look like "no connections yet" to a user who has 10 saved.
    if (loadError) {
      return (
        <div className="placeholder">
          <div className="error">
            {t("app.couldNotLoadConnections", { error: loadError })}
          </div>
          <button className="btn" onClick={() => void refresh()}>
            {t("app.retry")}
          </button>
        </div>
      );
    }
    if (conns.length === 0) {
      return (
        <Onboarding
          onNewConnection={() => setEditing("new")}
          onOpenMcp={openMcpSettings}
        />
      );
    }
    const safetyFallback = safetyError ? (
      <div className="error">
        {t("app.loadSafetyFailed", { error: safetyError })}{" "}
        <button className="btn small" onClick={() => selectedId && loadSafety(selectedId)}>
          {t("app.retry")}
        </button>
      </div>
    ) : (
      <div className="muted">{t("app.loading")}</div>
    );

    // Every workbench view needs the global connection context. With no connection selected,
    // the rail stays reachable but the current view asks for one explicit selection.
    const needsConn = (
      <ConnectionPicker
        connections={conns}
        onSelect={(id) => selectConnection(id, area)}
        onNew={startNewConnection}
      />
    );

    return (
      <>
        {selected && (
          <header className="main-head ds-workbench-head" data-tauri-drag-region="deep">
            <div className="ds-workbench-title">
              <div className="ds-title-line app-title-line">
                <EngineMark engine={selected.engine} />
                <strong>{selected.name || t("app.unnamed")}</strong>
                {selected.env && <span className={`env-chip env-${selected.env}`}>{selected.env}</span>}
                <span className="ds-meta-dot" />
                <span className="app-title-meta">{selected.database}</span>
                {area === "workspace" && selectedTable && (
                  <>
                    <span className="ds-meta-dot" />
                    <span className="app-title-meta">{tableLabel(selected.engine, selectedTable)}</span>
                  </>
                )}
              </div>
            </div>
            <div className="main-head-actions ds-control-row">
              <button
                ref={agentButtonRef}
                type="button"
                className={`btn small main-agent-toggle${showAgentDock ? " active" : ""}`}
                onClick={toggleAgentDock}
                title={t("tabs.agent")}
                aria-label={t("tabs.agent")}
                aria-pressed={showAgentDock}
              >
                <Icon name="sidebar" />
                {unseen > 0 && (
                  <span className="workbench-rail-count">
                    {unseen > 9 ? "9+" : unseen}
                  </span>
                )}
              </button>
            </div>
          </header>
        )}

        {selected && area === "workspace" && (
          <WorkbenchDocumentStrip
            documents={selectedDocuments}
            activeId={activeDocumentId}
            engine={selected.engine}
            supportsSql={supportsSql}
            onActivate={setActiveDocumentId}
            onClose={closeDocument}
            onNewQuery={openQueryDocument}
            onOpenActivity={() => openStableDocument("activity")}
          />
        )}

        <section className={`tab-body workbench-canvas area-${area}`}>
          {!selected ? (
            needsConn
          ) : area === "dashboard" ? (
            <Dashboards
              connection={selected}
              focusId={dashboardFocusId}
              onFocusConsumed={consumeDashboardFocus}
              onOpenAgent={openAgentDock}
            />
          ) : !activeDocument ? (
            <div className="workbench-empty">
              <Icon name={supportsSql ? "play" : "list"} />
              <span className="muted">
                {supportsSql ? t("tabs.sql") : t("tabs.documents")}
              </span>
              <button className="btn primary" onClick={openQueryDocument}>
                <Icon name="plus" />
                {supportsSql ? t("tabs.sql") : t("tabs.documents")}
              </button>
            </div>
          ) : activeDocument.kind === "data" ? (
            safety ? (
              <TableData
                key={activeDocument.id}
                connection={selected}
                table={activeDocument.table}
                safety={safety}
              />
            ) : (
              safetyFallback
            )
          ) : activeDocument.kind === "schema" ? (
            <SchemaExplorer
              key={activeDocument.id}
              connection={selected}
              selectedTable={null}
              onOpenTable={(table) => openTableDocument(selected, table)}
            />
          ) : activeDocument.kind === "sql" ? (
            safety ? (
              <Sql
                key={activeDocument.id}
                connection={selected}
                safety={safety}
                draft={activeDocument.draft}
                setDraft={setActiveQueryDraft}
              />
            ) : (
              safetyFallback
            )
          ) : activeDocument.kind === "documents" ? (
            <Documents
              key={activeDocument.id}
              connection={selected}
              draft={activeDocument.draft}
            />
          ) : (
            <Activity
              key={activeDocument.id}
              connection={selected}
              onLoadSql={loadSql}
              initialAuditOpen={legacyAuditOpen.current}
              onInitialAuditOpenConsumed={() => {
                legacyAuditOpen.current = false;
              }}
            />
          )}
        </section>
      </>
    );
  }

  const showMcpBadge = mcpBadgeReady && !!mcpBadge && !settingsOpen;
  const showUpdateBadge = !!availableUpdate && !settingsOpen;
  const mcpBadgeLabel =
    mcpBadge === "server" ? t("mcp.badgeServerDown") : t("mcp.badgeDisconnected");
  const mcpBadgeTitle =
    mcpBadge === "server"
      ? t("mcp.badgeServerDownTitle")
      : t("mcp.badgeDisconnectedTitle");
  return (
    <div
      className={`app${IS_MACOS ? " platform-macos" : ""}${
        showAgentDock ? " agent-open" : ""
      }`}
      style={{
        gridTemplateColumns: `48px ${sidebarW}px 5px minmax(0, 1fr) ${
          showAgentDock ? "minmax(300px, 340px)" : "0px"
        }`,
      }}
    >
      <WorkbenchRail
        area={settingsOpen ? null : area}
        dashboardAvailable={!selected || supportsSql}
        settingsOpen={settingsOpen}
        account={
          <WorkspaceAccount compact onScopeChanged={reloadWorkspaceScope} />
        }
        onArea={(next) => {
          setSettingsOpen(false);
          setEditing(null);
          setSchemaDiffGroupKey(null);
          setArea(next);
          if (next === "workspace" && selected) {
            openStableDocument("schema");
          }
        }}
        onSettings={() => {
          setSettingsSection(undefined);
          setSettingsOpen(true);
          setSchemaDiffGroupKey(null);
        }}
      />
      {area === "dashboard" && !settingsOpen && editing === null && !activeSchemaGroup ? (
        <DashboardSidebar
          workspaceHeader={
            <WorkspaceSwitcher onNew={startNewConnection} onChanged={reloadWorkspaceScope} />
          }
          connections={conns}
          selectedId={selectedId}
          focusId={dashboardFocusId}
          onSelectConnection={(id) => selectConnection(id, "dashboard")}
          onFocus={setDashboardFocusId}
        />
      ) : (
        <DatabaseExplorer
          workspaceHeader={
            <WorkspaceSwitcher
              onNew={startNewConnection}
              onChanged={reloadWorkspaceScope}
            />
          }
          connections={conns}
          selectedId={selectedId}
          selectedTableKey={selectedTable ? tableKey(selectedTable) : null}
          activeSchemaGroupKey={schemaDiffGroupKey}
          onSelectConn={(id) => selectConnection(id, "workspace")}
          onOpenTable={openTableDocument}
          onOpenSchemaDiff={(group) => {
            setArea("workspace");
            setEditing(null);
            setSettingsOpen(false);
            setSchemaDiffGroupKey(group.key);
          }}
          onEdit={(conn) => {
            setEditing(conn);
            setSettingsOpen(false);
            setSchemaDiffGroupKey(null);
          }}
          onDeleted={async (id) => {
            await refresh();
            if (selectedId === id) {
              initializedConnection.current = null;
              setSelectedId(null);
              setDocuments([]);
              setActiveDocumentId(null);
            }
            if (schemaDiffGroupKey) setSchemaDiffGroupKey(null);
            setEditing((current) => {
              if (current && current !== "new" && current.id === id) return null;
              return current;
            });
          }}
          onConnectionUpdated={(updated) => {
            setConns((current) =>
              current.map((conn) => (conn.id === updated.id ? updated : conn)),
            );
            setEditing((current) => {
              if (current && current !== "new" && current.id === updated.id) {
                return updated;
              }
              return current;
            });
          }}
        />
      )}
      <div
        className="sidebar-resizer"
        title={t("app.dragResize")}
        onMouseDown={startSidebarDrag}
        onDoubleClick={() => {
          setSidebarW(SIDEBAR_DEFAULT);
          localStorage.setItem("sidebarW", String(SIDEBAR_DEFAULT));
        }}
      />
      <main className="main">
        {renderMain()}
        {(showUpdateBadge || showMcpBadge) && (
          <div className="ds-attention-stack">
            {showUpdateBadge && (
              <button
                className="ds-attention-badge ds-tone-trust"
                onClick={openUpdateSettings}
                title={t("updates.badgeTitle")}
                aria-label={t("updates.badgeTitle")}
              >
                <Icon name="download" />
                <span>{t("updates.badge", { version: availableUpdate?.version ?? "" })}</span>
              </button>
            )}
            {showMcpBadge && (
              <button
                className={
                  "ds-attention-badge " +
                  (mcpBadge === "server" ? "ds-tone-danger" : "ds-tone-risk")
                }
                onClick={openMcpSettings}
                title={mcpBadgeTitle}
                aria-label={mcpBadgeTitle}
              >
                <Icon name={mcpBadge === "server" ? "alert" : "database"} />
                <span>{mcpBadgeLabel}</span>
              </button>
            )}
          </div>
        )}
      </main>
      {showAgentDock && selected && (
        <aside
          ref={agentDockRef}
          className="agent-dock"
          aria-label={t("tabs.agent")}
          role={agentOverlay ? "dialog" : undefined}
          aria-modal={agentOverlay || undefined}
        >
          <header className="agent-dock-head">
            <strong>{t("tabs.agent")}</strong>
            <div className="ds-control-row">
              <button
                ref={agentHistoryButtonRef}
                type="button"
                className={`btn small${agentHistoryOpen ? " active" : ""}`}
                onClick={() => setAgentHistoryOpen((open) => !open)}
                title={t("agentChat.toggleThreads")}
                aria-label={t("agentChat.toggleThreads")}
                aria-expanded={agentHistoryOpen}
                aria-controls="agent-chat-history"
              >
                <Icon name="history" />
              </button>
              <button
                type="button"
                className="btn small"
                onClick={() => setAgentLogOpen(true)}
                title={t("agentChat.logsFor", {
                  name: selected.name || t("app.unnamed"),
                })}
                aria-label={t("agentChat.logsFor", {
                  name: selected.name || t("app.unnamed"),
                })}
              >
                <Icon name="list" />
                {unseen > 0 && (
                  <span className="tab-dot">{unseen > 9 ? "9+" : unseen}</span>
                )}
              </button>
              <button
                ref={agentCloseRef}
                type="button"
                className="btn small"
                onClick={closeAgentDock}
                title={t("common.close")}
                aria-label={t("common.close")}
              >
                <Icon name="close" />
              </button>
            </div>
          </header>
          <div className="agent-dock-body">
            <AgentChat
              compact
              historyOpen={agentHistoryOpen}
              onHistoryOpenChange={(open) => {
                setAgentHistoryOpen(open);
                if (!open) {
                  window.requestAnimationFrame(() =>
                    agentHistoryButtonRef.current?.focus(),
                  );
                }
              }}
              onOpenLogs={() => setAgentLogOpen(true)}
              onOpenMcpSettings={openMcpSettings}
              selectedConnection={selected}
            />
          </div>
        </aside>
      )}
      {agentLogOpen && selected && (
        <AgentLogDialog
          connection={selected}
          onDashboardSaved={openDashboard}
          onClose={() => setAgentLogOpen(false)}
        />
      )}
    </div>
  );
}
