import { useEffect, useRef, useState } from "react";
import { getSafety, listConnections, setSafety as ipcSetSafety } from "./ipc/commands";
import type { CatalogTable, ConnectionProfile, SafetySettings } from "./ipc/types";
import { errMessage } from "./ipc/types";
import { tableKey } from "./lib/tableRef";
import { ConnectionForm, DatabaseExplorer } from "./screens/Connections";
import TableData from "./screens/Tables";
import Sql from "./screens/Sql";
import History from "./screens/History";
import Audit from "./screens/Audit";
import Migrations from "./screens/Migrations";
import Onboarding from "./screens/Onboarding";
import Settings from "./screens/Settings";
import AgentResultView from "./components/AgentResultView";
import { ToastProvider, useToast } from "./components/Toast";
import { AgentFeedProvider, useAgentFeed } from "./lib/agentFeed";
import { useI18n, type I18nKey } from "./lib/i18n";

// App-level tabs. Agent is a live feed/result surface, connection-independent; the rest
// are per-connection data views. Migrations lives in the sidebar; Settings behind ⚙.
type Tab = "data" | "sql" | "agent" | "history" | "audit";
// Agent is last: it's connection-independent, so it sits apart from the per-connection
// data tabs (the .agent-tab class pushes it right with a divider via shared CSS).
const TABS: Tab[] = ["data", "sql", "history", "audit", "agent"];
const TAB_LABELS: Record<Tab, I18nKey> = {
  data: "tabs.data",
  sql: "tabs.sql",
  history: "tabs.history",
  audit: "tabs.audit",
  agent: "tabs.agent",
};

// `null` = not editing; "new" = blank form; a profile = edit that profile.
type Editing = ConnectionProfile | "new" | null;

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
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedTable, setSelectedTable] = useState<CatalogTable | null>(null);
  const [editing, setEditing] = useState<Editing>(null);
  const [safety, setSafety] = useState<SafetySettings | null>(null);
  const [safetyError, setSafetyError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("data");
  const [sqlDraft, setSqlDraft] = useState("SELECT 1;");
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<"mcp" | "safety" | undefined>(undefined);
  const [migrationsOpen, setMigrationsOpen] = useState(false);

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

  // #10: quick read↔write toggle from the main head — the most-flipped setting, otherwise
  // buried in Settings ▸ Safety. Persists via the IPC set_safety (not the local setter) so
  // the backend gate actually changes, then refreshes. Enabling writes (the risky direction)
  // confirms first. `togglingWrites` reflects the pending state so the control isn't double-hit.
  const [togglingWrites, setTogglingWrites] = useState(false);
  async function toggleWrites() {
    if (!selectedId || !safety || togglingWrites) return;
    const enabling = !safety.allowWrites;
    if (
      enabling &&
      !window.confirm(
        t("app.confirmAllowWrites", { name: selected?.name || t("app.thisConnection") }),
      )
    )
      return;
    setTogglingWrites(true);
    try {
      await ipcSetSafety(selectedId, { ...safety, allowWrites: enabling });
      refreshSafety();
    } catch (e) {
      toast(errMessage(e), "error");
    } finally {
      setTogglingWrites(false);
    }
  }

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

  function renderMain() {
    if (settingsOpen) {
      return (
        <Settings
          connection={selected}
          initialSection={settingsSection}
          refreshSafety={refreshSafety}
          onClose={() => setSettingsOpen(false)}
          onOpenAgent={() => {
            refreshSafety();
            setSettingsOpen(false);
            setTab("agent");
          }}
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
            onDeleted={async (id) => {
              await refresh();
              if (selectedId === id) {
                setSelectedId(null);
                setSelectedTable(null);
              }
              setEditing(null);
            }}
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

    // Data/SQL/History/Audit need a connection; Agent is connection-independent and always
    // renders its live feed. With no connection selected, the tab bar still shows so Agent
    // stays reachable — the data tabs fall back to a "pick a connection" placeholder.
    const needsConn = (
      <div className="placeholder muted">{t("app.connectionRequired")}</div>
    );

    return (
      <>
        {selected && (
          <header className="main-head">
            <div>
              <strong>{selected.name || t("app.unnamed")}</strong>
              <span className="muted">
                {" "}
                {selected.engine} · {selected.database}
              </span>
              {safety && (
                <button
                  className={
                    "write-toggle" + (safety.allowWrites ? " writes" : " readonly")
                  }
                  disabled={togglingWrites}
                  onClick={() => void toggleWrites()}
                  title={
                    safety.allowWrites
                      ? t("app.writeWritesAllowedTitle")
                      : t("app.writeReadOnlyTitle")
                  }
                >
                  {togglingWrites
                    ? "..."
                    : safety.allowWrites
                      ? t("app.writeWritesAllowed")
                      : t("app.writeReadOnly")}
                </button>
              )}
            </div>
            <button className="btn small" onClick={() => setEditing(selected)}>
              {t("app.edit")}
            </button>
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
          {tab === "agent" && <AgentResultView onOpenMcpSettings={openMcpSettings} />}
          {tab === "data" &&
            (!selected ? (
              needsConn
            ) : !safety ? (
              safetyFallback
            ) : selectedTable ? (
              <TableData connection={selected} table={selectedTable} safety={safety} />
            ) : (
              <div className="placeholder muted">
                {t("app.noTableSelected")}
              </div>
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
          {tab === "history" &&
            (selected ? <History connection={selected} onLoadSql={loadSql} /> : needsConn)}
          {tab === "audit" &&
            (selected ? <Audit connectionId={selected.id} /> : needsConn)}
        </section>
      </>
    );
  }

  return (
    <div className="app" style={{ gridTemplateColumns: `${sidebarW}px 5px 1fr` }}>
      <DatabaseExplorer
        connections={conns}
        selectedId={selectedId}
        selectedTableKey={selectedTable ? tableKey(selectedTable) : null}
        migrationsOpen={migrationsOpen}
        onSelectConn={(id) => {
          setSelectedId(id);
          setSelectedTable(null);
          setEditing(null);
          setSettingsOpen(false);
          setMigrationsOpen(false);
          setTab("data");
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
          setEditing("new");
          setSettingsOpen(false);
          setMigrationsOpen(false);
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
    </div>
  );
}
