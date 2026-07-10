import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import {
  getSafety,
  listConnections,
  mcpPlatforms,
  mcpRuntimeStatus,
} from "./ipc/commands";
import type {
  CatalogTable,
  ConnectionProfile,
  Dashboard,
  SafetySettings,
} from "./ipc/types";
import { errMessage } from "./ipc/types";
import { buildConnectionSections, type SchemaConnectionGroup } from "./lib/schemaDiff";
import { tableKey, tableLabel } from "./lib/tableRef";
import { ConnectionForm, DatabaseExplorer } from "./screens/Connections";
import TableData from "./screens/Tables";
import SchemaExplorer from "./screens/Schema";
import Sql from "./screens/Sql";
import Dashboards from "./screens/Dashboards";
import Activity from "./screens/Activity";
import Migrations from "./screens/Migrations";
import Onboarding from "./screens/Onboarding";
import Settings from "./screens/Settings";
import AgentResultView from "./components/AgentResultView";
import EngineMark from "./components/EngineMark";
import { Icon } from "./components/Icon";
import { ToastProvider, useToast } from "./components/Toast";
import { AgentFeedProvider, useAgentFeed } from "./lib/agentFeed";
import { useI18n, type I18nKey } from "./lib/i18n";

// App-level tabs. Agent is a live feed/result surface, connection-independent; the rest
// are per-connection data views. Migrations lives in the sidebar; Settings behind ⚙.
type Tab = "data" | "schema" | "sql" | "dashboard" | "agent" | "activity";
// Agent is last: it's connection-independent, so it sits apart from the per-connection
// data tabs (the .agent-tab class pushes it right with a divider via shared CSS).
const TABS: Tab[] = ["data", "schema", "sql", "dashboard", "activity", "agent"];
const TAB_LABELS: Record<Tab, I18nKey> = {
  data: "tabs.data",
  schema: "tabs.schema",
  sql: "tabs.sql",
  dashboard: "tabs.dashboard",
  activity: "tabs.activity",
  agent: "tabs.agent",
};

// `null` = not editing; "new" = blank form; a profile = edit that profile.
type Editing = ConnectionProfile | "new" | null;
type McpBadgeState = "server" | "disconnected";

export default function App() {
  return (
    <ToastProvider>
      <AgentFeedProvider>
        <Shell />
      </AgentFeedProvider>
    </ToastProvider>
  );
}

const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 520;
const SIDEBAR_DEFAULT = 240;
const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;

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
    return (TABS as string[]).includes(saved ?? "") ? (saved as Tab) : "data";
  });
  const [sqlDraft, setSqlDraft] = useState("SELECT 1;");
  const [dashboardFocusId, setDashboardFocusId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<
    "mcp" | "safety" | "updates" | undefined
  >(undefined);
  const [migrationsOpen, setMigrationsOpen] = useState(false);
  const [mcpBadge, setMcpBadge] = useState<McpBadgeState | null>(null);
  const [mcpBadgeReady, setMcpBadgeReady] = useState(false);
  const [mcpRefreshTick, setMcpRefreshTick] = useState(0);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const updateCheckInFlight = useRef(false);
  const lastUpdateCheckAt = useRef(0);

  const openDashboard = useCallback((dashboard: Dashboard) => {
    setSelectedId(dashboard.connectionId);
    setSelectedTable(null);
    setEditing(null);
    setSettingsOpen(false);
    setMigrationsOpen(false);
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

  function refresh(): Promise<ConnectionProfile[]> {
    return listConnections()
      .then((cs) => {
        setConns(cs);
        setLoadError(null);
        return cs;
      })
      .catch((e) => {
        setLoadError(errMessage(e));
        return [];
      });
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

  // Global MCP attention signal: if the local server/bridge is down, or no supported
  // agent platform has DopeDB registered, keep a small setup affordance visible.
  useEffect(() => {
    let cancelled = false;

    async function refreshMcpBadge() {
      try {
        const runtime = await mcpRuntimeStatus();
        if (cancelled) return;
        let serverNeedsAttention = !runtime.httpRunning || !runtime.bridgeRunning;
        if (serverNeedsAttention) {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
          if (cancelled) return;
          const retry = await mcpRuntimeStatus();
          serverNeedsAttention = !retry.httpRunning || !retry.bridgeRunning;
        }
        const platforms = await mcpPlatforms();
        if (cancelled) return;
        const hasConnectedPlatform = platforms.some((p) => p.connected);
        setMcpBadge(
          serverNeedsAttention ? "server" : hasConnectedPlatform ? null : "disconnected",
        );
      } catch {
        if (!cancelled) setMcpBadge("disconnected");
      } finally {
        if (!cancelled) setMcpBadgeReady(true);
      }
    }

    void refreshMcpBadge();
    const iv = window.setInterval(() => void refreshMcpBadge(), 30000);
    return () => {
      cancelled = true;
      window.clearInterval(iv);
    };
  }, [mcpRefreshTick]);

  // Nudge (not hijack) when a new result lands while the user is off the Agent tab. Skip
  // the mount baseline so it doesn't fire on load; guard on result id so it fires once per
  // result; throttle to one toast per 30s so a burst of agent queries yields a single nudge.
  const seenResultId = useRef<number | null>(null);
  const surfaceInit = useRef(true);
  const lastToastAt = useRef(0);
  // Tab button refs so ArrowLeft/Right moves DOM focus with the selection (roving tabindex).
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  useEffect(() => {
    if (surfaceInit.current) {
      surfaceInit.current = false;
      seenResultId.current = latest?.id ?? null;
      return;
    }
    if (latest && latest.id !== seenResultId.current) {
      seenResultId.current = latest.id;
      const now = Date.now();
      if (tab !== "agent" && now - lastToastAt.current > 30000) {
        lastToastAt.current = now;
        toast(t("app.toastAgentQuery"));
      }
    }
  }, [latest, tab, toast]);

  // Clear the unseen count when the Agent tab is shown (on switch, and as new events land).
  useEffect(() => {
    if (tab === "agent" && unseen > 0) markSeen();
  }, [tab, unseen, markSeen]);

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
    setSqlDraft(sql);
    setTab("sql");
  }

  function openMcpSettings() {
    setSettingsSection("mcp");
    setSettingsOpen(true);
    setMigrationsOpen(false);
    setEditing(null);
  }

  function openUpdateSettings() {
    setSettingsSection("updates");
    setSettingsOpen(true);
    setMigrationsOpen(false);
    setEditing(null);
  }

  function refreshMcpConnectionState() {
    setMcpRefreshTick((tick) => tick + 1);
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
    setMigrationsOpen(false);
    setDashboardFocusId(null);
    setTab(nextTab);
  }

  function startNewConnection() {
    setEditing("new");
    setSettingsOpen(false);
    setMigrationsOpen(false);
  }

  function renderMain() {
    if (settingsOpen) {
      return (
        <Settings
          connection={selected}
          initialSection={settingsSection}
          refreshSafety={refreshSafety}
          onMcpChanged={refreshMcpConnectionState}
          availableUpdate={availableUpdate}
          onUpdateChecked={syncAvailableUpdate}
          onClose={() => setSettingsOpen(false)}
        />
      );
    }
    if (migrationsOpen && selected) {
      return (
        <div className="main-view">
          <Migrations connection={selected} onClose={() => setMigrationsOpen(false)} />
        </div>
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

    // Per-connection views need a connection; Agent is connection-independent and always
    // renders its live feed. With no connection selected, the tab bar still shows so Agent
    // stays reachable — the data tabs fall back to a "pick a connection" placeholder.
    const needsConn = (
      <ConnectionPicker
        connections={conns}
        onSelect={(id) => selectConnection(id, tab === "agent" ? "data" : tab)}
        onNew={startNewConnection}
      />
    );

    return (
      <>
        {selected && tab !== "agent" && (
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
            {unseen > 0 && (
              <div className="main-policy ds-command-group">
                <button
                  className="btn small agent-jump"
                  onClick={() => setTab("agent")}
                  title={t("app.agentUnseen", { count: unseen })}
                  aria-label={t("app.agentUnseen", { count: unseen })}
                >
                  <Icon name="alert" />
                  {unseen}
                </button>
              </div>
            )}
          </header>
        )}

        <nav className="tabs" role="tablist">
          {TABS.map((tabId) => (
            <button
              key={tabId}
              ref={(el) => {
                tabRefs.current[tabId] = el;
              }}
              role="tab"
              aria-selected={tabId === tab}
              // Roving tabindex: only the active tab is in the Tab order; arrows move within.
              tabIndex={tabId === tab ? 0 : -1}
              className={
                (tabId === tab ? "tab active" : "tab") +
                (tabId === "agent" ? " agent-tab" : "")
              }
              onClick={() => setTab(tabId)}
              // Arrow keys move focus+selection across TABS, wrapping — mirrors the sidebar tree.
              onKeyDown={(e) => {
                if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
                e.preventDefault();
                const i = TABS.findIndex((x) => x === tab);
                const d = e.key === "ArrowRight" ? 1 : -1;
                const next = TABS[(i + d + TABS.length) % TABS.length];
                setTab(next);
                tabRefs.current[next]?.focus();
              }}
            >
              {t(TAB_LABELS[tabId])}
              {tabId === "agent" && unseen > 0 && (
                <span className="tab-dot">{unseen > 9 ? "9+" : unseen}</span>
              )}
            </button>
          ))}
        </nav>

        <section className="tab-body" role="tabpanel">
          {tab === "agent" && <AgentResultView onDashboardSaved={openDashboard} />}
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
                title={t("tabs.data")}
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
          {tab === "dashboard" &&
            (selected ? (
              <Dashboards
                connection={selected}
                focusId={dashboardFocusId}
                onFocusConsumed={consumeDashboardFocus}
                onOpenAgent={() => setTab("agent")}
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
    <div className="app" style={{ gridTemplateColumns: `${sidebarW}px 5px 1fr` }}>
      <DatabaseExplorer
        connections={conns}
        selectedId={selectedId}
        selectedTableKey={selectedTable ? tableKey(selectedTable) : null}
        migrationsOpen={migrationsOpen}
        onSelectConn={(id) => {
          selectConnection(id);
        }}
        onOpenTable={(conn, t) => {
          setSelectedId(conn.id);
          setSelectedTable(t);
          setEditing(null);
          setSettingsOpen(false);
          setMigrationsOpen(false);
          setTab("data");
        }}
        onOpenMigrations={(conn) => {
          setSelectedId(conn.id);
          setSelectedTable(null);
          setEditing(null);
          setSettingsOpen(false);
          setMigrationsOpen(true);
        }}
        onNew={() => {
          startNewConnection();
        }}
        onEdit={(conn) => {
          setEditing(conn);
          setSettingsOpen(false);
          setMigrationsOpen(false);
        }}
        onDeleted={async (id) => {
          await refresh();
          if (selectedId === id) {
            setSelectedId(null);
            setSelectedTable(null);
            setMigrationsOpen(false);
          }
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
        onOpenSettings={() => {
          setSettingsSection(undefined);
          setSettingsOpen(true);
          setMigrationsOpen(false);
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
      <main className="main">{renderMain()}</main>
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
    </div>
  );
}
