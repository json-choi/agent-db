// Connection-scoped Agent workspace: the sidebar's selected database is the sole context,
// while this screen owns conversation history, provider/model controls, and the composer.
// MCP result/audit details stay available through a secondary log action instead of a tab.
import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { deleteChatThread } from "../../ipc/commands";
import type {
  AgentModel,
  AgentProvider,
  ChatThread,
  CliInfo,
  ConnectionProfile,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { Icon } from "../../components/Icon";
import Skeleton from "../../components/Skeleton";
import { useToast } from "../../components/Toast";
import {
  connectionThreads,
  loadProviderStrings,
  saveProviderString,
  useAgentChat,
} from "../../lib/agentChat";
import { useAgentFeed, type AgentActivity } from "../../lib/agentFeed";
import { isNearBottom } from "../../lib/autoScroll";
import {
  agentChatMessagesQuery,
  agentChatThreadsQuery,
  agentCliDetectionQuery,
  agentModelsQuery,
  mcpRuntimeStatusQuery,
  qk,
} from "../../lib/queries";
import { useI18n, type I18nKey } from "../../lib/i18n";
import { fullTime, relTime } from "../../lib/relTime";
import "./agentChat.css";

const CODEX_CONSENT_KEY = "dopedb:agentChat:codexConsent";
const RAIL_OPEN_KEY = "dopedb:agentChat:railOpen";
// Distance (px) from the bottom of the message list within which a streaming chunk still
// auto-scrolls the view — beyond this, the user has deliberately scrolled up to read back.
const AUTO_SCROLL_THRESHOLD = 80;

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
// CliInfo carries no user-facing text (see ipc/types.ts) — the subscription-login
// disclosure is a frontend i18n string keyed by provider instead.
const PROVIDER_NOTE_KEYS: Record<AgentProvider, I18nKey> = {
  claude: "agentChat.claudeNote",
  codex: "agentChat.codexNote",
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

function latestActivityDuringTurn(
  feed: AgentActivity[],
  msg: DisplayMessage,
): AgentActivity | null {
  if (!msg.turnStartIso) return null;
  return feed.find((item) => item.iso >= msg.turnStartIso!) ?? null;
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
      <p className="muted">{t(PROVIDER_NOTE_KEYS[info.id])}</p>
      <pre className="agent-chat-command" onClick={() => onCopy(command)}>
        {command}
      </pre>
      <div className="ds-toolbar ds-control-row">
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

export default function AgentChat({
  onOpenLogs,
  onOpenMcpSettings,
  selectedConnection,
}: {
  onOpenLogs: () => void;
  onOpenMcpSettings: () => void;
  selectedConnection: ConnectionProfile;
}) {
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
  const mcpRuntime = useQuery(mcpRuntimeStatusQuery());
  const clis = detect.data ?? [];
  const threadsQuery = useQuery(agentChatThreadsQuery());
  const threads = threadsQuery.data ?? [];
  const scopedThreads = useMemo(
    () => connectionThreads(threads, selectedConnection.id),
    [threads, selectedConnection.id],
  );
  const activeThread =
    scopedThreads.find((thread) => thread.id === threadId) ?? null;
  const provider: AgentProvider = activeThread ? activeThread.provider : draftProvider;

  const selected = clis.find((c) => c.id === provider) ?? null;
  const selectedReady = !!selected?.installed && !!selected?.authenticated;

  // A conversation belongs to exactly one global database context. If the user switches
  // the sidebar selection while another database's thread is active, start a clean draft
  // instead of silently showing/sending against the old scope.
  useEffect(() => {
    if (!threadsQuery.isSuccess || threadId === null) return;
    const current = threads.find((thread) => thread.id === threadId);
    if (!current || current.connectionId !== selectedConnection.id) openThread(null);
  }, [openThread, selectedConnection.id, threadId, threads, threadsQuery.isSuccess]);

  const [codexConsent, setCodexConsent] = useState(
    () => localStorage.getItem(CODEX_CONSENT_KEY) === "1",
  );
  // Orca-style project navigation stays visible by default, but users can collapse it and
  // the preference remains local to this device.
  const [railOpen, setRailOpen] = useState(() => localStorage.getItem(RAIL_OPEN_KEY) !== "0");
  function toggleRail() {
    setRailOpen((open) => {
      localStorage.setItem(RAIL_OPEN_KEY, open ? "0" : "1");
      return !open;
    });
  }
  const [draft, setDraft] = useState("");
  const [submitError, setSubmitError] = useState<string | null>(null);
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

  // Opening a (different) conversation always starts at its latest message, regardless of
  // where the list happened to be scrolled to before.
  useEffect(() => {
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
  }, [threadId]);

  // Within the same conversation (streamed text growing the last bubble), only follow the
  // tail if the user was already reading near the bottom — otherwise a token arriving while
  // they've scrolled up to reread history would yank the view back down.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    if (isNearBottom(el, AUTO_SCROLL_THRESHOLD)) el.scrollTo({ top: el.scrollHeight });
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

  async function submit() {
    const text = draft.trim();
    if (!text || busy) return;
    setSubmitError(null);
    try {
      await send(
        text,
        provider,
        selectedConnection.id,
        model || undefined,
        effort || undefined,
      );
      setDraft("");
    } catch (error) {
      const message = errMessage(error);
      setSubmitError(message);
      toast(message, "error");
    }
  }

  const needsConsent = provider === "codex" && selectedReady && !codexConsent;
  const showsAuthWarning = provider === "claude" && selected?.authMethod === "claude.ai";
  const providerReady = selectedReady && !needsConsent;
  const mcpReady = !!mcpRuntime.data?.httpRunning;
  const chatReady = providerReady && mcpReady;
  // Independent of `busy` (which also covers the post-done cache flush): Cancel is only
  // meaningful while a turn is actually still streaming.
  const showCancel = !!pendingTurn && !pendingTurn.done && pendingTurn.threadId === threadId;

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
            ) : scopedThreads.length === 0 ? (
              <div className="muted agent-chat-thread-empty">
                {t("agentChat.threadListEmpty")}
              </div>
            ) : (
              scopedThreads.map((th) => (
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
              <label
                className="agent-chat-provider-switcher"
                title={t("agentChat.providerSwitchHint")}
              >
                <span className="agent-chat-provider-label">
                  {t("agentChat.providerLabel")}
                </span>
                <span className="agent-chat-provider-select">
                  <select
                    aria-label={t("agentChat.providerSwitchHint")}
                    value={provider}
                    onChange={(event) => {
                      const nextProvider = event.target.value as AgentProvider;
                      // A thread's CLI session cannot change provider. Selecting another
                      // agent therefore opens a fresh draft for that provider.
                      if (nextProvider === provider) return;
                      openThread(null);
                      setDraftProvider(nextProvider);
                    }}
                  >
                    {clis.map((info) => (
                      <option key={info.id} value={info.id}>
                        {info.name}
                      </option>
                    ))}
                  </select>
                  <Icon name="chevronDown" />
                </span>
              </label>
            )}
            <span
              className="agent-chat-connection-chip"
              title={`${selectedConnection.engine} · ${selectedConnection.database}`}
            >
              <Icon name="database" />
              <span className="agent-chat-context-label">{t("agentChat.contextLabel")}</span>
              <strong>{selectedConnection.name || t("app.unnamed")}</strong>
              {selectedConnection.env && (
                <span className={`env-chip env-${selectedConnection.env}`}>
                  {selectedConnection.env}
                </span>
              )}
            </span>
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

              {providerReady && mcpRuntime.isPending && <Skeleton lines={2} />}

              {providerReady && !mcpRuntime.isPending && !mcpReady && (
                <div className="card ds-card-stack ds-tone-danger agent-chat-onboard-card">
                  <div className="ds-card-title-row">
                    <Icon name="alert" />
                    <strong>{t("agentChat.mcpUnavailable")}</strong>
                  </div>
                  <p className="muted">
                    {t("agentChat.mcpUnavailableBody")}
                    {mcpRuntime.data?.error ? ` ${mcpRuntime.data.error}` : ""}
                  </p>
                  <div className="ds-toolbar ds-control-row">
                    <button
                      className="btn small"
                      onClick={() => void mcpRuntime.refetch()}
                      disabled={mcpRuntime.isFetching}
                    >
                      {t("common.refresh")}
                    </button>
                    <button className="btn small" onClick={onOpenMcpSettings}>
                      {t("agentChat.openMcpSettings")}
                    </button>
                  </div>
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
                ) : messages.length === 0 ? (
                  <div className="agent-chat-empty">
                    <Icon name="database" />
                    <strong>
                      {t("agentChat.emptyTitle", {
                        name: selectedConnection.name || t("app.unnamed"),
                      })}
                    </strong>
                    <span>{t("agentChat.emptyHint")}</span>
                    <div className="agent-chat-suggestions">
                      {([
                        "agentChat.suggestionSchema",
                        "agentChat.suggestionTrend",
                        "agentChat.suggestionTables",
                      ] as I18nKey[]).map((key) => (
                        <button key={key} type="button" onClick={() => setDraft(t(key))}>
                          {t(key)}
                        </button>
                      ))}
                    </div>
                  </div>
                ) : (
                  messages.map((msg) => {
                    const tools = toolsDuringTurn(feed, msg);
                    const activity = latestActivityDuringTurn(feed, msg);
                    const pendingLabel =
                      activity?.kind === "call"
                        ? t("agentChat.runningTool", { tool: activity.tool })
                        : activity?.kind === "result"
                          ? t("agentChat.summarizing")
                          : t("agentChat.pending");
                    return (
                      <div key={msg.id} className={`agent-chat-msg ${msg.role}`}>
                        <div className="agent-chat-bubble">
                          {msg.pending && !msg.text ? (
                            <span className="loading">{pendingLabel}</span>
                          ) : (
                            msg.text
                          )}
                        </div>
                        {msg.pending && !!msg.text && (
                          <span className="muted agent-chat-progress">{pendingLabel}</span>
                        )}
                        {msg.error && <div className="error">{msg.error}</div>}
                        {tools.length > 0 && (
                          <button
                            className="btn small agent-chat-tool-chip"
                            onClick={onOpenLogs}
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
                {submitError && (
                  <div className="error agent-chat-model-error" role="alert">
                    {submitError}
                  </div>
                )}
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
                  onChange={(e) => {
                    setDraft(e.target.value);
                    setSubmitError(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      void submit();
                    }
                  }}
                  placeholder={t("agentChat.composerPlaceholder", {
                    name: selectedConnection.name || t("app.unnamed"),
                  })}
                  aria-label={t("agentChat.composerPlaceholder", {
                    name: selectedConnection.name || t("app.unnamed"),
                  })}
                  disabled={busy}
                />
                <div className="agent-chat-composer-controls ds-control-row">
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
                  <button
                    className="btn primary"
                    disabled={busy || !draft.trim()}
                    onClick={() => void submit()}
                  >
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
