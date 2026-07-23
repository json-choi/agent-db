import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
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
import Dashboards from "./screens/Dashboards";
import Activity from "./screens/Activity";
import SchemaDiff from "./screens/SchemaDiff";
import Onboarding from "./screens/Onboarding";
import Settings from "./screens/Settings";
import AgentChat from "./screens/AgentChat";
import AgentLogDialog from "./components/AgentLogDialog";
import EngineMark from "./components/EngineMark";
import { Icon } from "./components/Icon";
import { ToastProvider, useToast } from "./components/Toast";
import WorkspaceAccount from "./components/WorkspaceAccount";
import WorkspaceSwitcher from "./components/WorkspaceSwitcher";
import { AgentChatProvider } from "./lib/agentChat";
import { AgentFeedProvider, useAgentFeed } from "./lib/agentFeed";
import { useI18n, type I18nKey } from "./lib/i18n";

// Primary workbench destinations live in the icon rail. Agent is a persistent right-hand
// dock, so every surface shares the one connection selected in the database explorer.
type Tab = "data" | "schema" | "sql" | "dashboard" | "documents" | "activity";
const ALL_TABS: Tab[] = ["data", "schema", "sql", "dashboard", "documents", "activity"];

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

// Compact workbench rail: keeps the high-frequency destinations visible without
// stealing space from the database tree. The selected connection remains the
// single context for every destination.
function WorkbenchRail({
  tab,
  visibleTabs,
  agentOpen,
  agentAvailable,
  settingsOpen,
  unseen,
  agentButtonRef,
  onTab,
  onAgent,
  onSettings,
}: {
  tab: Tab | null;
  visibleTabs: Tab[];
  agentOpen: boolean;
  agentAvailable: boolean;
  settingsOpen: boolean;
  unseen: number;
  agentButtonRef: RefObject<HTMLButtonElement | null>;
  onTab: (tab: Tab) => void;
  onAgent: () => void;
  onSettings: () => void;
}) {
  const { t } = useI18n();
  const items: Array<{
    id: Tab;
    icon: "database" | "table" | "play" | "dashboard" | "chart" | "list";
    label: I18nKey;
  }> = [
    { id: "data", icon: "database", label: "tabs.data" },
    { id: "schema", icon: "table", label: "tabs.schema" },
    { id: "sql", icon: "play", label: "tabs.sql" },
    { id: "dashboard", icon: "dashboard", label: "tabs.dashboard" },
    { id: "documents", icon: "list", label: "tabs.documents" },
    { id: "activity", icon: "chart", label: "tabs.activity" },
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
            ".workbench-rail-button:not(:disabled)",
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
      <div className="workbench-rail-brand">d</div>
      <div className="workbench-rail-items">
        {items.filter((item) => visibleTabs.includes(item.id)).map((item) => (
          <button
            key={item.id}
            type="button"
            className={`workbench-rail-button${tab === item.id ? " active" : ""}`}
            onClick={() => onTab(item.id)}
            title={t(item.label)}
            aria-label={t(item.label)}
            aria-current={tab === item.id ? "page" : undefined}
          >
            <Icon name={item.icon} />
          </button>
        ))}
        <span className="workbench-rail-separator" />
        <button
          ref={agentButtonRef}
          type="button"
          className={`workbench-rail-button${agentOpen ? " active" : ""}`}
          onClick={onAgent}
          title={t("tabs.agent")}
          aria-label={t("tabs.agent")}
          aria-pressed={agentOpen}
          disabled={!agentAvailable}
        >
          <Icon name="sidebar" />
          {unseen > 0 && (
            <span className="workbench-rail-count">{unseen > 9 ? "9+" : unseen}</span>
          )}
        </button>
      </div>
      <div className="workbench-rail-bottom">
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
  const [selectedTable, setSelectedTable] = useState<CatalogTable | null>(null);
  const [editing, setEditing] = useState<Editing>(null);
  const [safety, setSafety] = useState<SafetySettings | null>(null);
  const [safetyError, setSafetyError] = useState<string | null>(null);
  // A user who last had the old Audit tab open should land in its expanded details
  // after the two top-level tabs are consolidated into Activity.
  const legacyAuditOpen = useRef(localStorage.getItem("tab") === "audit");
  const [tab, setTab] = useState<Tab>(() => {
    const saved = localStorage.getItem("tab");
    if (saved === "history" || saved === "audit") return "activity";
    if (saved === "chat" || saved === "agent") return "data";
    return (ALL_TABS as string[]).includes(saved ?? "") ? (saved as Tab) : "data";
  });
  const [agentDockOpen, setAgentDockOpen] = useState(
    () => localStorage.getItem("agentDockOpen") !== "0",
  );
  const [agentOverlay, setAgentOverlay] = useState(
    () => window.matchMedia("(max-width: 900px)").matches,
  );
  const [sqlDraft, setSqlDraft] = useState("SELECT 1;");
  // Mirrors sqlDraft for MongoDB connections, where an Activity row click routes here
  // instead of the (absent) SQL tab — see loadSql below.
  const [docDraft, setDocDraft] = useState<string | null>(null);
  const [dashboardFocusId, setDashboardFocusId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<
    "mcp" | "safety" | "updates" | undefined
  >(undefined);
  const [schemaDiffGroupKey, setSchemaDiffGroupKey] = useState<string | null>(null);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const [agentLogOpen, setAgentLogOpen] = useState(false);
  const agentButtonRef = useRef<HTMLButtonElement | null>(null);
  const agentCloseRef = useRef<HTMLButtonElement | null>(null);
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
    setSelectedTable(null);
    setEditing(null);
    setSettingsOpen(false);
    setSchemaDiffGroupKey(null);
    setDashboardFocusId(dashboard.id);
    setTab("dashboard");
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

  // Persist tab/selectedId so a restart resumes where the user left off (mirrors sidebarW).
  useEffect(() => {
    localStorage.setItem("tab", tab);
  }, [tab]);
  useEffect(() => {
    if (selectedId) localStorage.setItem("selectedId", selectedId);
    else localStorage.removeItem("selectedId");
  }, [selectedId]);

  const selected = conns.find((c) => c.id === selectedId) ?? null;
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
  // driver's capabilities (fail-closed: no match hides SQL). No connection selected keeps
  // the full SQL tab set (existing behavior) and hides Documents.
  const driversQ = useQuery(driversQuery());
  // While the driver list is still loading, keep the SQL tab set — deciding on an
  // empty list would flash Documents and kick a restored "sql" tab back to "data".
  const supportsSql =
    !selected || !driversQ.data || hasCapability(driversQ.data, selected, "sql");
  const visibleTabs = useMemo(
    () =>
      ALL_TABS.filter((t) => {
        if (t === "sql" || t === "dashboard") return supportsSql;
        if (t === "documents") return !supportsSql;
        return true;
      }),
    [supportsSql],
  );
  // A restored selectedId with conns still loading means visibleTabs is not final
  // yet — resetting now would clobber a persisted "documents" tab on every launch.
  // Once conns arrive, a selectedId that matches nothing is stale, not pending.
  const selectionPending = selectedId !== null && !selected && conns.length === 0;
  useEffect(() => {
    if (selectionPending) return;
    if (!visibleTabs.includes(tab)) setTab("data");
  }, [selectionPending, visibleTabs, tab]);

  useEffect(() => {
    const media = window.matchMedia("(max-width: 900px)");
    const sync = () => setAgentOverlay(media.matches);
    media.addEventListener("change", sync);
    return () => media.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    if (!showAgentDock || !agentOverlay) return;
    const close = () => {
      localStorage.setItem("agentDockOpen", "0");
      setAgentDockOpen(false);
      window.requestAnimationFrame(() => agentButtonRef.current?.focus());
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
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
    setSelectedTable(null);
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
    // A mongo connection has no SQL tab (see visibleTabs) — route the same Activity
    // "load" action to Documents instead so the row click isn't silently dropped.
    if (supportsSql) {
      setSqlDraft(sql);
      setTab("sql");
    } else {
      setDocDraft(sql);
      setTab("documents");
    }
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

  function syncAvailableUpdate(update: Update | null) {
    lastUpdateCheckAt.current = Date.now();
    setAvailableUpdate(update);
  }

  function selectConnection(id: string, nextTab: Tab = "data") {
    setSelectedId(id);
    setSelectedTable(null);
    setEditing(null);
    setSettingsOpen(false);
    setSchemaDiffGroupKey(null);
    setDashboardFocusId(null);
    setAgentLogOpen(false);
    setTab(nextTab);
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
        onSelect={(id) => selectConnection(id, tab)}
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
                {selectedTable && (
                  <>
                    <span className="ds-meta-dot" />
                    <span className="app-title-meta">{tableLabel(selected.engine, selectedTable)}</span>
                  </>
                )}
              </div>
            </div>
          </header>
        )}

        <section className="tab-body">
          {tab === "data" &&
            (!selected ? (
              needsConn
            ) : selectedTable ? (
              safety ? (
                <TableData connection={selected} table={selectedTable} safety={safety} />
              ) : (
                safetyFallback
              )
            ) : (
              <SchemaExplorer
                connection={selected}
                selectedTable={selectedTable}
                onOpenTable={(table) => {
                  setSelectedTable(table);
                  setTab("data");
                }}
              />
            ))}
          {tab === "schema" &&
            (selected ? (
              <SchemaExplorer
                connection={selected}
                selectedTable={selectedTable}
                onOpenTable={(table) => {
                  setSelectedTable(table);
                  setTab("data");
                }}
              />
            ) : (
              needsConn
            ))}
          {tab === "sql" &&
            (!selected ? (
              needsConn
            ) : safety ? (
              <Sql
                connection={selected}
                safety={safety}
                draft={sqlDraft}
                setDraft={setSqlDraft}
              />
            ) : (
              safetyFallback
            ))}
          {tab === "documents" &&
            (selected ? (
              <Documents key={selected.id} connection={selected} draft={docDraft} />
            ) : (
              needsConn
            ))}
          {tab === "dashboard" &&
            (selected ? (
              <Dashboards
                connection={selected}
                focusId={dashboardFocusId}
                onFocusConsumed={consumeDashboardFocus}
                onOpenAgent={openAgentDock}
              />
            ) : (
              needsConn
            ))}
          {tab === "activity" &&
            (selected ? (
              <Activity
                key={selected.id}
                connection={selected}
                onLoadSql={loadSql}
                initialAuditOpen={legacyAuditOpen.current}
                onInitialAuditOpenConsumed={() => {
                  legacyAuditOpen.current = false;
                }}
              />
            ) : (
              needsConn
            ))}
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
      className={`app${showAgentDock ? " agent-open" : ""}`}
      style={{
        gridTemplateColumns: `48px ${sidebarW}px 5px minmax(0, 1fr) ${
          showAgentDock ? "minmax(300px, 360px)" : "0px"
        }`,
      }}
    >
      <WorkbenchRail
        tab={settingsOpen ? null : tab}
        visibleTabs={visibleTabs}
        agentOpen={showAgentDock}
        agentAvailable={!!selected}
        settingsOpen={settingsOpen}
        unseen={unseen}
        agentButtonRef={agentButtonRef}
        onTab={(next) => {
          if (next === "sql") preloadSqlEditor();
          setSettingsOpen(false);
          setEditing(null);
          setSchemaDiffGroupKey(null);
          setTab(next);
        }}
        onAgent={() => {
          if (settingsOpen || editing !== null || !agentDockOpen) {
            setSettingsOpen(false);
            setEditing(null);
            setSchemaDiffGroupKey(null);
            openAgentDock();
            return;
          }
          localStorage.setItem("agentDockOpen", "0");
          setAgentDockOpen(false);
        }}
        onSettings={() => {
          setSettingsSection(undefined);
          setSettingsOpen(true);
          setSchemaDiffGroupKey(null);
        }}
      />
      <DatabaseExplorer
        workspaceAccount={<WorkspaceAccount onScopeChanged={reloadWorkspaceScope} />}
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
        onSelectConn={(id) => {
          selectConnection(id, tab);
        }}
        onOpenTable={(conn, t) => {
          setSelectedId(conn.id);
          setSelectedTable(t);
          setEditing(null);
          setSettingsOpen(false);
          setSchemaDiffGroupKey(null);
          setTab("data");
        }}
        onOpenSchemaDiff={(group) => {
          setSelectedTable(null);
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
            setSelectedId(null);
            setSelectedTable(null);
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
                ref={agentCloseRef}
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
                type="button"
                className="btn small"
                onClick={() => {
                  localStorage.setItem("agentDockOpen", "0");
                  setAgentDockOpen(false);
                }}
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
