// In-app agent chat, Codex-desktop-style: a thread rail on the left, a centered
// conversation column, and an onboarding gate (install/auth/consent for the claude/codex
// CLI) that still runs before the composer appears. The subscription/read-only disclosures
// live in the onboarding cards (CliInfo.note, consent card) rather than a permanent banner.
import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { deleteChatThread } from "../../ipc/commands";
import type { AgentModel, AgentProvider, ChatThread, CliInfo } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { Icon } from "../../components/Icon";
import Skeleton from "../../components/Skeleton";
import { useToast } from "../../components/Toast";
import {
  loadProviderStrings,
  saveProviderString,
  useAgentChat,
} from "../../lib/agentChat";
import { useAgentFeed, type AgentActivity } from "../../lib/agentFeed";
import {
  agentChatMessagesQuery,
  agentChatThreadsQuery,
  agentCliDetectionQuery,
  agentModelsQuery,
  qk,
} from "../../lib/queries";
import { useI18n } from "../../lib/i18n";
import { fullTime, relTime } from "../../lib/relTime";
import "./agentChat.css";

const CODEX_CONSENT_KEY = "dopedb:agentChat:codexConsent";
const RAIL_OPEN_KEY = "dopedb:agentChat:railOpen";

// ponytail: one literal command per platform, no OS branching — the design docs only
// greenlit these two; add Windows-specific variants once they're actually verified.
const INSTALL_COMMANDS: Record<AgentProvider, string> = {
  claude: "curl -fsSL https://claude.ai/install.sh | bash",
  codex: "npm install -g @openai/codex",
};
const LOGIN_COMMANDS: Record<AgentProvider, string> = {
  claude: "claude",
  codex: "codex login",
};

// One row in the message list: either a persisted ChatMessageRecord (verbatim) or the live
// pending turn's optimistic user echo / streaming assistant buffer.
interface DisplayMessage {
  id: string;
  role: "user" | "assistant";
  text: string;
  error?: string | null;
  pending?: boolean;
  turnStartIso?: string; // set only on the live turn's bubble — the agentFeed tool slice below
}

// A turn's tool calls are whatever the agent feed logged from this turn's start onward. Only
// the live/just-finished turn carries turnStartIso, so past history never grows tool chips.
function toolsDuringTurn(feed: AgentActivity[], msg: DisplayMessage): AgentActivity[] {
  if (!msg.turnStartIso) return [];
  return feed.filter((item) => item.kind === "result" && item.iso >= msg.turnStartIso!);
}

function ProviderOnboardCard({
  info,
  onCopy,
  onCheckAgain,
  checking,
}: {
  info: CliInfo;
  onCopy: (text: string) => void;
  onCheckAgain: () => void;
  checking: boolean;
}) {
  const { t } = useI18n();
  const command = info.installed ? LOGIN_COMMANDS[info.id] : INSTALL_COMMANDS[info.id];
  return (
    <div className="card ds-card-stack agent-chat-onboard-card">
      <div className="ds-card-title-row">
        <Icon name={info.installed ? "alert" : "circleSlash"} />
        <strong>
          {info.installed
            ? t("agentChat.loginTitle", { name: info.name })
            : t("agentChat.installTitle", { name: info.name })}
        </strong>
      </div>
      <p className="muted">{info.note}</p>
      <pre className="agent-chat-command" onClick={() => onCopy(command)}>
        {command}
      </pre>
      <div className="ds-toolbar">
        <span className="muted">
          {info.installed ? t("agentChat.loginHint") : t("agentChat.installHint")}
        </span>
        <button className="btn small" onClick={() => onCopy(command)}>
          {t("common.copy")}
        </button>
        <button className="btn small" onClick={onCheckAgain} disabled={checking}>
          {t("agentChat.checkAgain")}
        </button>
      </div>
    </div>
  );
}

function ThreadRow({
  thread,
  active,
  busy,
  deleting,
  providerLabel,
  onOpen,
  onDelete,
}: {
  thread: ChatThread;
  active: boolean;
  busy: boolean;
  deleting: boolean;
  providerLabel: string;
  onOpen: () => void;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  const deleteLabel = busy ? t("agentChat.deleteThreadBusy") : t("agentChat.deleteThread");
  return (
    <div
      className={`agent-chat-thread-row ds-object-row${active ? " active" : ""}`}
      role="button"
      tabIndex={0}
      aria-selected={active}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
    >
      <span className="agent-chat-thread-meta">
        <span className="agent-chat-thread-title">
          {thread.title || t("agentChat.threadUntitled")}
        </span>
        <span className="agent-chat-thread-sub">
          <span>{providerLabel}</span>
          <span className="ds-meta-dot" />
          <span title={fullTime(thread.updatedAt)}>{relTime(thread.updatedAt)}</span>
        </span>
      </span>
      <button
        type="button"
        className="btn small agent-chat-thread-delete"
        title={deleteLabel}
        aria-label={deleteLabel}
        disabled={busy || deleting}
        onClick={(e) => {
          e.stopPropagation();
          onDelete();
        }}
      >
        <Icon name="trash" />
      </button>
    </div>
  );
}

export default function AgentChat({ onOpenAgent }: { onOpenAgent: () => void }) {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const { feed } = useAgentFeed();
  const {
    threadId,
    openThread,
    draftProvider,
    setDraftProvider,
    pendingTurn,
    busy,
    send,
    cancelActive,
  } = useAgentChat();

  const detect = useQuery(agentCliDetectionQuery());
  const clis = detect.data ?? [];
  const threadsQuery = useQuery(agentChatThreadsQuery());
  const threads = threadsQuery.data ?? [];
  const activeThread = threads.find((th) => th.id === threadId) ?? null;
  const provider: AgentProvider = activeThread ? activeThread.provider : draftProvider;

  const selected = clis.find((c) => c.id === provider) ?? null;
  const selectedReady = !!selected?.installed && !!selected?.authenticated;

  const [codexConsent, setCodexConsent] = useState(
    () => localStorage.getItem(CODEX_CONSENT_KEY) === "1",
  );
  // The thread rail is an on-demand drawer, not a permanent column — closed by default so
  // the conversation gets the full width, remembered across sessions.
  const [railOpen, setRailOpen] = useState(() => localStorage.getItem(RAIL_OPEN_KEY) === "1");
  function toggleRail() {
    setRailOpen((open) => {
      localStorage.setItem(RAIL_OPEN_KEY, open ? "0" : "1");
      return !open;
    });
  }
  const [draft, setDraft] = useState("");
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);

  // Unsent composer text belongs to whichever thread it was typed for — never carry it
  // across a thread switch, or it can get sent into the wrong conversation.
  useEffect(() => {
    setDraft("");
  }, [threadId]);

  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");

  // Before a conversation starts, jump to whichever CLI is actually usable — a user with
  // only Codex installed shouldn't land on Claude's onboarding by default.
  useEffect(() => {
    if (threadId !== null || selectedReady) return;
    const ready = clis.find((c) => c.installed && c.authenticated);
    if (ready) setDraftProvider(ready.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clis, threadId, selectedReady]);

  // Seed the picker from the active thread's own row (its last-used model/effort), or from
  // this draft provider's remembered choice. Re-seeds only on a thread switch or provider
  // change while drafting — not on every keystroke inside the pickers themselves.
  useEffect(() => {
    if (activeThread) {
      setModel(activeThread.model ?? "");
      setEffort(activeThread.effort ?? "");
    } else {
      setModel(loadProviderStrings("model")[provider]);
      setEffort(loadProviderStrings("effort")[provider]);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [threadId, provider]);

  const modelsQuery = useQuery({ ...agentModelsQuery(provider), enabled: selectedReady });
  const models: AgentModel[] = modelsQuery.data ?? [];
  const selectedModel = models.find((m) => m.id === model) ?? null;

  // A catalog model is always a concrete choice (never free text), so once the list loads,
  // fall back to the top-priority entry instead of leaving the select blank.
  useEffect(() => {
    if (!model && models.length > 0) onModelChange(models[0].id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [model, models]);

  function onModelChange(id: string) {
    setModel(id);
    saveProviderString("model", provider, id);
    const entry = models.find((m) => m.id === id);
    if (entry && effort && !entry.efforts.includes(effort)) {
      setEffort("");
      saveProviderString("effort", provider, "");
    }
  }

  function onEffortChange(v: string) {
    setEffort(v);
    saveProviderString("effort", provider, v);
  }

  const messagesQuery = useQuery(agentChatMessagesQuery(threadId));
  const persisted = messagesQuery.data ?? [];
  const messages = useMemo<DisplayMessage[]>(() => {
    const list: DisplayMessage[] = persisted.map((m) => ({
      id: m.id,
      role: m.role,
      text: m.text,
      error: m.error,
    }));
    if (pendingTurn && pendingTurn.threadId === threadId) {
      list.push({ id: `${pendingTurn.turnId}-user`, role: "user", text: pendingTurn.userText });
      list.push({
        id: pendingTurn.turnId,
        role: "assistant",
        text: pendingTurn.assistantText,
        pending: !pendingTurn.done,
        error: pendingTurn.error,
        turnStartIso: pendingTurn.turnStartIso,
      });
    }
    return list;
  }, [persisted, pendingTurn, threadId]);

  useEffect(() => {
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
  }, [messages]);

  function copy(text: string) {
    void navigator.clipboard.writeText(text);
    toast(t("common.copied"));
  }

  function providerLabel(id: AgentProvider): string {
    return clis.find((c) => c.id === id)?.name ?? id;
  }

  async function handleDeleteThread(thread: ChatThread) {
    if (!window.confirm(t("agentChat.deleteThreadConfirm"))) return;
    setDeletingId(thread.id);
    try {
      await deleteChatThread(thread.id);
      queryClient.removeQueries({ queryKey: qk.chatMessages(thread.id) });
      await queryClient.invalidateQueries({ queryKey: qk.chatThreads() });
      if (threadId === thread.id) openThread(null);
    } catch (e) {
      toast(errMessage(e), "error");
    } finally {
      setDeletingId(null);
    }
  }

  function submit() {
    const text = draft.trim();
    if (!text || busy) return;
    send(text, provider, model || undefined, effort || undefined);
    setDraft("");
  }

  const needsConsent = provider === "codex" && selectedReady && !codexConsent;
  const showsAuthWarning = provider === "claude" && selected?.authMethod === "claude.ai";
  const chatReady = selectedReady && !needsConsent;
  const showCancel = busy && pendingTurn?.threadId === threadId;

  return (
    <div className="agent-chat-screen screen">
      <div className="agent-chat-layout">
        {railOpen && (
        <aside className="agent-chat-rail">
          <button
            type="button"
            className="btn agent-chat-new-btn"
            onClick={() => {
              openThread(null);
              setDraft("");
            }}
          >
            <Icon name="plus" />
            {t("agentChat.newChat")}
          </button>
          <div className="agent-chat-thread-list">
            {threadsQuery.isPending ? (
              <Skeleton lines={4} />
            ) : threads.length === 0 ? (
              <div className="muted agent-chat-thread-empty">
                {t("agentChat.threadListEmpty")}
              </div>
            ) : (
              threads.map((th) => (
                <ThreadRow
                  key={th.id}
                  thread={th}
                  active={th.id === threadId}
                  busy={!!pendingTurn && pendingTurn.threadId === th.id && !pendingTurn.done}
                  deleting={deletingId === th.id}
                  providerLabel={providerLabel(th.provider)}
                  onOpen={() => openThread(th.id)}
                  onDelete={() => void handleDeleteThread(th)}
                />
              ))
            )}
          </div>
        </aside>
        )}

        <div className="agent-chat-main">
          <div className="agent-chat-topbar">
            <button
              type="button"
              className="btn small"
              aria-expanded={railOpen}
              aria-label={t("agentChat.toggleThreads")}
              title={t("agentChat.toggleThreads")}
              onClick={toggleRail}
            >
              <Icon name="sidebar" />
            </button>
            {!railOpen && (
              <button
                type="button"
                className="btn small"
                aria-label={t("agentChat.newChat")}
                title={t("agentChat.newChat")}
                onClick={() => {
                  openThread(null);
                  setDraft("");
                }}
              >
                <Icon name="plus" />
              </button>
            )}
            {clis.length > 0 && (
            <div className="agent-chat-provider-tabs" role="tablist">
              {clis.map((info) => (
                <button
                  key={info.id}
                  role="tab"
                  aria-selected={info.id === provider}
                  className={
                    info.id === provider
                      ? "agent-chat-provider-seg active"
                      : "agent-chat-provider-seg"
                  }
                  onClick={() => {
                    // A thread's provider is fixed (its CLI session can't move), so
                    // clicking a tab mid-conversation starts a fresh draft instead.
                    if (threadId === null && info.id === draftProvider) return;
                    openThread(null);
                    setDraftProvider(info.id);
                  }}
                >
                  {info.name}
                </button>
              ))}
            </div>
            )}
          </div>

          {detect.isPending ? (
            <Skeleton lines={3} />
          ) : detect.error ? (
            <div className="error" role="alert">
              {t("agentChat.detectFailed", { error: errMessage(detect.error) })}{" "}
              <button className="btn small" onClick={() => void detect.refetch()}>
                {t("common.refresh")}
              </button>
            </div>
          ) : !chatReady ? (
            <div className="agent-chat-onboard ds-card-grid">
              {selected && !selectedReady && (
                <ProviderOnboardCard
                  info={selected}
                  onCopy={copy}
                  onCheckAgain={() => void detect.refetch()}
                  checking={detect.isFetching}
                />
              )}

              {needsConsent && (
                <div className="card ds-card-stack ds-tone-risk agent-chat-consent">
                  <div className="ds-card-title-row">
                    <Icon name="alert" />
                    <strong>{t("agentChat.codexConsentTitle")}</strong>
                  </div>
                  <p className="muted">{t("agentChat.codexConsentBody")}</p>
                  <button
                    className="btn primary small"
                    onClick={() => {
                      localStorage.setItem(CODEX_CONSENT_KEY, "1");
                      setCodexConsent(true);
                    }}
                  >
                    {t("agentChat.codexConsentAgree")}
                  </button>
                </div>
              )}
            </div>
          ) : (
            <div className="agent-chat-conversation">
              {showsAuthWarning && (
                <div
                  className="card ds-card-row ds-tone-risk agent-chat-auth-warning"
                  title={t("agentChat.authWarningBody")}
                >
                  <Icon name="alert" />
                  <div>
                    <strong>{t("agentChat.authWarningTitle")}</strong>
                  </div>
                </div>
              )}

              <div className="agent-chat-messages" role="log" ref={listRef}>
                {threadId !== null && messagesQuery.isPending ? (
                  <Skeleton lines={4} />
                ) : (
                  messages.map((msg) => {
                    const tools = toolsDuringTurn(feed, msg);
                    return (
                      <div key={msg.id} className={`agent-chat-msg ${msg.role}`}>
                        <div className="agent-chat-bubble">
                          {msg.pending && !msg.text ? (
                            <span className="loading">{t("agentChat.pending")}</span>
                          ) : (
                            msg.text
                          )}
                        </div>
                        {msg.error && <div className="error">{msg.error}</div>}
                        {tools.length > 0 && (
                          <button
                            className="btn small agent-chat-tool-chip"
                            onClick={onOpenAgent}
                          >
                            <Icon name="database" />
                            {t("agentChat.usedTools", { count: tools.length })}
                          </button>
                        )}
                      </div>
                    );
                  })
                )}
              </div>

              <div className="card agent-chat-composer">
                {modelsQuery.error && (
                  <div className="error agent-chat-model-error">
                    {t("agentChat.modelLoadFailed", { error: errMessage(modelsQuery.error) })}{" "}
                    <button className="btn small" onClick={() => void modelsQuery.refetch()}>
                      {t("common.refresh")}
                    </button>
                  </div>
                )}
                <textarea
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      submit();
                    }
                  }}
                  placeholder={t("agentChat.composerPlaceholder")}
                  aria-label={t("agentChat.composerPlaceholder")}
                  disabled={busy}
                />
                <div className="agent-chat-composer-controls">
                  <select
                    aria-label={t("agentChat.modelLabel")}
                    value={model}
                    disabled={modelsQuery.isPending || models.length === 0}
                    onChange={(e) => onModelChange(e.target.value)}
                  >
                    {models.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.name}
                      </option>
                    ))}
                  </select>
                  <select
                    aria-label={t("agentChat.effortLabel")}
                    value={effort}
                    disabled={!selectedModel || selectedModel.efforts.length === 0}
                    onChange={(e) => onEffortChange(e.target.value)}
                  >
                    <option value="">{t("agentChat.effortDefault")}</option>
                    {(selectedModel?.efforts ?? []).map((level) => (
                      <option key={level} value={level}>
                        {level}
                      </option>
                    ))}
                  </select>
                  <span className="ds-toolbar-spacer" />
                  {showCancel && (
                    <button className="btn small" onClick={cancelActive}>
                      {t("common.cancel")}
                    </button>
                  )}
                  <button className="btn primary" disabled={busy || !draft.trim()} onClick={submit}>
                    {t("agentChat.send")}
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
