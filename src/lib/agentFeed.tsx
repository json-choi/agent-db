// App-level agent activity feed. The MCP listeners live here (started once from App via
// AgentFeedProvider) instead of inside the Mcp screen, so agent tool calls/results are
// captured even when that screen isn't mounted — the app is the visualizer, always on.
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import type { QueryResult } from "../ipc/types";

const CAP = 200; // ponytail: capped in-memory ring; add persistence only if history matters
const KEEP_ROWS = 25; // retain full rows only for the newest N results; older keep metadata only

export interface AgentActivity {
  id: number;
  ts: string; // short display string (toLocaleTimeString)
  iso: string; // full timestamp, for a hover tooltip — display string alone is ambiguous across midnight
  kind: "call" | "result";
  tool: string;
  detail: string;
  error?: boolean;
  result?: QueryResult;
  rowsDropped?: boolean; // rows evicted to bound memory; metadata (columns/rowCount) kept
  sql?: string;
  connection?: string; // source connection name, for the result provenance header
}

function resultOf(p: Record<string, unknown>): QueryResult | undefined {
  if (Array.isArray(p.columns) && Array.isArray(p.rows)) {
    return {
      columns: p.columns as string[],
      rows: p.rows as unknown[][],
      rowCount: typeof p.rowCount === "number" ? p.rowCount : (p.rows as unknown[][]).length,
      truncated: !!p.truncated,
      durationMs: typeof p.durationMs === "number" ? p.durationMs : 0,
    };
  }
  return undefined;
}

function resultDetail(p: Record<string, unknown>): string {
  if (p.error) return `error: ${p.error}`;
  if (typeof p.rowCount === "number") return `${p.rowCount} rows`;
  if (typeof p.count === "number") return `${p.count} items`;
  return "ok";
}

interface AgentFeedValue {
  feed: AgentActivity[];
  latest: AgentActivity | null; // most recent activity carrying a result
  unseen: number;
  markSeen: () => void;
}

const Ctx = createContext<AgentFeedValue | null>(null);

export function useAgentFeed(): AgentFeedValue {
  const v = useContext(Ctx);
  if (!v) throw new Error("useAgentFeed must be used within AgentFeedProvider");
  return v;
}

export function AgentFeedProvider({ children }: { children: ReactNode }) {
  const [feed, setFeed] = useState<AgentActivity[]>([]);
  const [latest, setLatest] = useState<AgentActivity | null>(null);
  const [unseen, setUnseen] = useState(0);
  const idRef = useRef(0);

  useEffect(() => {
    const push = (a: Omit<AgentActivity, "id" | "ts" | "iso">) => {
      const now = new Date();
      const item: AgentActivity = { ...a, id: idRef.current++, ts: now.toLocaleTimeString(), iso: now.toISOString() };
      setFeed((f) => {
        // Bound memory: past the newest KEEP_ROWS results, drop rows but keep columns/rowCount
        // so the row stays clickable and shows a "re-run to view" note instead of an empty grid.
        // New objects (never mutate — items are shared with the selected-result state).
        let kept = 0;
        return [item, ...f].slice(0, CAP).map((a) => {
          if (!a.result || a.rowsDropped) return a;
          if (++kept <= KEEP_ROWS) return a;
          return { ...a, rowsDropped: true, result: { ...a.result, rows: [] } };
        });
      });
      if (item.result) setLatest(item);
      // One agent operation emits a call AND a result — count it once, on completion.
      if (item.kind === "result") setUnseen((n) => n + 1);
    };
    const p1 = listen<Record<string, unknown>>("agent:tool_call", (e) =>
      push({ kind: "call", tool: String(e.payload.tool ?? "?"), detail: String(e.payload.sql ?? e.payload.connection ?? "") }),
    ).catch((e) => console.error("agent feed listen failed:", e));
    const p2 = listen<Record<string, unknown>>("agent:result", (e) =>
      push({
        kind: "result",
        tool: String(e.payload.tool ?? "?"),
        detail: resultDetail(e.payload),
        error: !!e.payload.error,
        result: resultOf(e.payload),
        sql: typeof e.payload.sql === "string" ? e.payload.sql : undefined,
        connection: typeof e.payload.connection === "string" ? e.payload.connection : undefined,
      }),
    ).catch((e) => console.error("agent feed listen failed:", e));
    return () => {
      // .catch above widens these to UnlistenFn | void, so guard before calling.
      void p1.then((u) => u && u());
      void p2.then((u) => u && u());
    };
  }, []);

  const markSeen = useCallback(() => setUnseen(0), []);

  return <Ctx.Provider value={{ feed, latest, unseen, markSeen }}>{children}</Ctx.Provider>;
}
