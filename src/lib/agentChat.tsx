// App-level in-app agent chat state. Mounted once at the App level (same spot as
// AgentFeedProvider) so a conversation survives leaving and returning to the Chat tab.
// Thread list and message history live in TanStack Query (the DB is the source of truth) —
// this provider only keeps the two things that must survive a screen unmount mid-turn: which
// thread is active, and the one globally-active turn's streaming buffer. The backend allows
// only one concurrent turn (see ChatSlot in agent/mod.rs), so `busy` is a single global flag.
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { cancelQuery, createChatThread, sendChatTurn } from "../ipc/commands";
import type { AgentProvider } from "../ipc/types";
import { errMessage } from "../ipc/types";
import { qk } from "./queries";

// The live, not-yet-persisted view of one turn: an optimistic user bubble plus the
// streaming assistant buffer. Cleared once the corresponding messages query has refetched
// the persisted rows, so there is no gap where neither the buffer nor the DB copy is shown.
export interface PendingTurn {
  threadId: string;
  turnId: string;
  userText: string;
  assistantText: string;
  turnStartIso: string; // start bound for the agentFeed tool-call slice of this turn
  done: boolean;
  error?: string;
}

interface AgentChatValue {
  threadId: string | null; // null = an unsaved draft conversation
  openThread: (id: string | null) => void;
  draftProvider: AgentProvider; // which CLI a not-yet-created draft will use
  setDraftProvider: (p: AgentProvider) => void;
  pendingTurn: PendingTurn | null;
  busy: boolean;
  send: (
    text: string,
    provider: AgentProvider,
    connectionId: string | null,
    model?: string,
    effort?: string,
  ) => void;
  cancelActive: () => void;
}

const Ctx = createContext<AgentChatValue | null>(null);

export function useAgentChat(): AgentChatValue {
  const v = useContext(Ctx);
  if (!v) throw new Error("useAgentChat must be used within AgentChatProvider");
  return v;
}

// Model/effort picks are per provider (a Claude model name means nothing to codex), so both
// persist under one localStorage key per provider. The composer seeds a draft conversation's
// initial picker state from these; an existing thread seeds from its own row instead.
type ProviderStrings = Record<AgentProvider, string>;

export function providerStorageKey(kind: "model" | "effort", p: AgentProvider): string {
  return `dopedb:agentChat:${kind}:${p}`;
}

export function loadProviderStrings(kind: "model" | "effort"): ProviderStrings {
  return {
    claude: localStorage.getItem(providerStorageKey(kind, "claude")) ?? "",
    codex: localStorage.getItem(providerStorageKey(kind, "codex")) ?? "",
  };
}

export function saveProviderString(kind: "model" | "effort", p: AgentProvider, v: string) {
  if (v) localStorage.setItem(providerStorageKey(kind, p), v);
  else localStorage.removeItem(providerStorageKey(kind, p));
}

export function AgentChatProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [threadId, setThreadId] = useState<string | null>(null);
  const [draftProvider, setDraftProvider] = useState<AgentProvider>("claude");
  const [pendingTurn, setPendingTurn] = useState<PendingTurn | null>(null);

  // Stays true through the post-done invalidate+clear flush (pendingTurn is only cleared to
  // null once the persisted rows are confirmed in cache below) — not just while streaming —
  // so a second send can't race the just-finished turn's rows into the messages cache.
  const busy = pendingTurn !== null;

  useEffect(() => {
    const p1 = listen<{ turnId: string; threadId: string; textChunk: string }>(
      "agent:chat_event",
      (e) => {
        setPendingTurn((p) =>
          p && p.turnId === e.payload.turnId
            ? { ...p, assistantText: p.assistantText + e.payload.textChunk }
            : p,
        );
      },
    ).catch((err) => console.error("chat event listen failed:", err));

    const p2 = listen<{ turnId: string; threadId: string; ok: boolean; error?: string }>(
      "agent:chat_done",
      (e) => {
        const { turnId, threadId: doneThreadId, ok, error } = e.payload;
        setPendingTurn((p) =>
          p && p.turnId === turnId
            ? { ...p, done: true, error: ok ? undefined : (error ?? "unknown error") }
            : p,
        );
        // Refetch the persisted rows (the backend inserted both the user and assistant
        // messages by now) before dropping the local buffer, so the bubble never disappears
        // and reappears — invalidateQueries awaits the refetch of any currently-mounted query.
        // The threads-list refetch only affects the sidebar (title/updated_at), so it runs
        // independently instead of gating how long the duplicate-bubble window stays open.
        void queryClient
          .invalidateQueries({ queryKey: qk.chatMessages(doneThreadId) })
          .then(() => {
            setPendingTurn((p) => (p && p.turnId === turnId ? null : p));
          });
        void queryClient.invalidateQueries({ queryKey: qk.chatThreads() });
      },
    ).catch((err) => console.error("chat done listen failed:", err));

    return () => {
      void p1.then((u) => u && u());
      void p2.then((u) => u && u());
    };
  }, [queryClient]);

  const openThread = useCallback((id: string | null) => {
    setThreadId(id);
  }, []);

  const send = useCallback(
    (
      text: string,
      provider: AgentProvider,
      connectionId: string | null,
      model?: string,
      effort?: string,
    ) => {
      if (busy || !text.trim()) return;
      const turnId = window.crypto.randomUUID();
      const turnStartIso = new Date().toISOString();
      // Captured once at call time: if the user navigates to a different thread while
      // createChatThread is still in flight below, this stays the draft's original (null)
      // value so the late setThreadId doesn't snap the view back to the new draft thread.
      const initialThreadId = threadId;

      async function run() {
        // A draft conversation gets its DB row only now, on its first message, so an
        // abandoned draft never leaves an empty thread in the sidebar.
        let tid = initialThreadId;
        if (!tid) {
          const thread = await createChatThread(provider, connectionId, model, effort);
          tid = thread.id;
          const createdTid = tid;
          setThreadId((current) => (current === initialThreadId ? createdTid : current));
          void queryClient.invalidateQueries({ queryKey: qk.chatThreads() });
        }
        setPendingTurn({
          threadId: tid,
          turnId,
          userText: text,
          assistantText: "",
          turnStartIso,
          done: false,
        });
        sendChatTurn(tid, text, turnId, model, effort).catch((e) => {
          setPendingTurn((p) =>
            p && p.turnId === turnId ? { ...p, done: true, error: errMessage(e) } : p,
          );
        });
      }
      void run();
    },
    [threadId, busy, queryClient],
  );

  const cancelActive = useCallback(() => {
    if (pendingTurn && !pendingTurn.done) void cancelQuery(pendingTurn.turnId);
  }, [pendingTurn]);

  return (
    <Ctx.Provider
      value={{
        threadId,
        openThread,
        draftProvider,
        setDraftProvider,
        pendingTurn,
        busy,
        send,
        cancelActive,
      }}
    >
      {children}
    </Ctx.Provider>
  );
}
