import React, { useRef, useEffect, useState, useCallback, useMemo, useLayoutEffect } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ChevronDown } from "lucide-react";
import type { Bot, Message, Followup } from "@apiari/types";
import { ChatInput } from "./ChatInput";
import { FollowupCard, FollowupIndicator } from "./FollowupCard";
import type { Attachment } from "./ChatInput";
import styles from "./ChatPanel.module.css";

export type { Attachment };

// ── Render prop types — all optional, fall back to built-in defaults ──────────

export interface RenderMessageProps {
  message: Message;
  bot: string;
}

export interface RenderInputProps {
  placeholder: string;
  loading: boolean;
  onSend: (text: string, attachments?: Attachment[]) => void;
  onCancel?: () => void;
  /** Number of messages queued behind the current in-flight response */
  queueCount: number;
}

export interface RenderMessageListProps {
  messages: Message[];
  bot: string;
  loading: boolean;
  streamingContent?: string;
  loadingStatus?: string;
  onCancel?: () => void;
}

interface Props {
  bot: string;
  botDescription?: string;
  botProvider?: string;
  botModel?: string;
  messages: Message[];
  messagesLoading: boolean;
  loading: boolean;
  loadingStatus?: string;
  streamingContent?: string;
  hasOlderHistory?: boolean;
  loadingOlderHistory?: boolean;
  onLoadOlderHistory?: () => Promise<void>;
  workerCount?: number;
  onWorkersToggle?: () => void;
  onCancel?: () => void;
  onSend: (text: string, attachments?: Attachment[]) => void;
  followups?: Followup[];
  workspace?: string;
  onFollowupCancelled?: () => void;
  bots?: Bot[];
  unread?: Record<string, number>;
  onSelectBot?: (name: string) => void;
  compactHeader?: boolean;
  /** Replace individual message bubbles. Receives the message + TTS controls. */
  renderMessage?: React.ComponentType<RenderMessageProps>;
  /** Replace the input area entirely. Receives send/cancel/voice props. */
  renderInput?: React.ComponentType<RenderInputProps>;
  /**
   * Replace the entire message list. Receives messages + loading state.
   * When provided: auto-scroll, scroll-to-bottom button, followup cards,
   * and older-history loading are all the consumer's responsibility.
   */
  renderMessageList?: React.ComponentType<RenderMessageListProps>;
}

interface QueuedMessage {
  text: string;
  attachments?: Attachment[];
}

export function ChatPanel({
  bot,
  botDescription,
  botProvider,
  botModel,
  messages,
  messagesLoading,
  loading,
  loadingStatus,
  streamingContent,
  hasOlderHistory = false,
  loadingOlderHistory = false,
  onLoadOlderHistory,
  onSend,
  workerCount,
  onWorkersToggle,
  onCancel,
  followups,
  workspace,
  onFollowupCancelled,
  bots,
  unread,
  onSelectBot,
  compactHeader = false,
  renderMessage: RenderMessage,
  renderInput: RenderInput,
  renderMessageList: RenderMessageList,
}: Props) {
  const messagesRef = useRef<HTMLDivElement>(null);
  const [showScrollBtn, setShowScrollBtn] = useState(false);
  const [messageQueue, setMessageQueue] = useState<QueuedMessage[]>([]);
  const isNearBottomRef = useRef(true);
  const restoringOlderHistoryRef = useRef(false);
  const loadingOlderRequestRef = useRef(false);
  const prevScrollStateRef = useRef({
    timelineLength: 0,
    pendingFollowups: 0,
    loading: false,
    streamingContent: "",
    loadingStatus: undefined as string | undefined,
  });

  // ── Message queue ──
  const handleSendOrQueue = useCallback(
    (text: string, attachments?: Attachment[]) => {
      if (loading) {
        setMessageQueue((q) => [...q, { text, attachments }]);
      } else {
        onSend(text, attachments);
      }
    },
    [loading, onSend],
  );

  // Clear queue on bot switch so queued messages don't leak across bots
  const prevBotRef = useRef(bot);
  useEffect(() => {
    if (prevBotRef.current !== bot) {
      setMessageQueue([]);
      prevBotRef.current = bot;
    }
  }, [bot]);

  // Drain queue when bot finishes responding
  const prevLoadingRef = useRef(loading);
  useEffect(() => {
    if (prevLoadingRef.current && !loading && messageQueue.length > 0) {
      const [next, ...rest] = messageQueue;
      setMessageQueue(rest);
      onSend(next.text, next.attachments);
    }
    prevLoadingRef.current = loading;
  }, [loading, messageQueue, onSend]);

  // ── Helpers ──

  const scrollToBottom = useCallback((behavior: ScrollBehavior) => {
    const container = messagesRef.current;
    if (!container) return;
    container.scrollTo({ top: container.scrollHeight, behavior });
  }, []);

  async function handleMessagesScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    isNearBottomRef.current = distanceFromBottom <= 120;
    setShowScrollBtn(distanceFromBottom > 40);

    if (
      el.scrollTop <= 80 &&
      hasOlderHistory &&
      !!onLoadOlderHistory &&
      !loadingOlderRequestRef.current &&
      !loadingOlderHistory &&
      !loading &&
      !messagesLoading
    ) {
      loadingOlderRequestRef.current = true;
      restoringOlderHistoryRef.current = true;
      const previousScrollHeight = el.scrollHeight;
      const previousScrollTop = el.scrollTop;

      try {
        await onLoadOlderHistory();
        requestAnimationFrame(() => {
          const container = messagesRef.current;
          if (container) {
            const nextScrollTop = container.scrollHeight - previousScrollHeight + previousScrollTop;
            container.scrollTop = Math.max(nextScrollTop, 0);
          }
          restoringOlderHistoryRef.current = false;
          loadingOlderRequestRef.current = false;
        });
      } catch {
        restoringOlderHistoryRef.current = false;
        loadingOlderRequestRef.current = false;
      }
    }
  }

  function handleScrollToBottom() {
    isNearBottomRef.current = true;
    setShowScrollBtn(false);
    scrollToBottom("smooth");
  }

  function formatTime(iso: string): string {
    const trimmed = iso.trim();
    if (!trimmed) return "";
    const normalized = trimmed.includes("T")
      ? trimmed.includes("Z") || trimmed.includes("+")
        ? trimmed
        : `${trimmed}Z`
      : trimmed;
    const date = new Date(normalized);
    if (Number.isNaN(date.getTime())) {
      return trimmed;
    }
    return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  }

  function renderAttachments(json: string | null) {
    if (!json) return null;
    try {
      const atts: Attachment[] = JSON.parse(json);
      return (
        <div className={styles.msgAttachments}>
          {atts.map((a, i) =>
            a.type.startsWith("image/") ? (
              <img key={i} src={a.dataUrl} alt={a.name} className={styles.msgImage} />
            ) : (
              <div key={i} className={styles.msgFile}>
                {a.name}
              </div>
            ),
          )}
        </div>
      );
    } catch {
      return null;
    }
  }

  // ── Timeline: merge fired followups into message feed ──

  type TimelineItem = { kind: "message"; msg: Message } | { kind: "followup"; followup: Followup };

  const { timeline, pendingFollowups } = useMemo(() => {
    const now = new Date();
    // Treat pending followups whose fires_at has elapsed as effectively fired
    const inlineFollowups = (followups ?? []).filter(
      (f) => f.status === "fired" || (f.status === "pending" && new Date(f.fires_at) <= now),
    );
    const pending = (followups ?? []).filter(
      (f) => f.status === "pending" && new Date(f.fires_at) > now,
    );

    const items: TimelineItem[] = [
      ...messages.map((msg): TimelineItem => ({ kind: "message", msg })),
      ...inlineFollowups.map((f): TimelineItem => ({ kind: "followup", followup: f })),
    ].sort((a, b) => {
      const timeA = new Date(
        a.kind === "message" ? a.msg.created_at : a.followup.fires_at,
      ).getTime();
      const timeB = new Date(
        b.kind === "message" ? b.msg.created_at : b.followup.fires_at,
      ).getTime();
      return timeA - timeB;
    });

    return { timeline: items, pendingFollowups: pending };
  }, [messages, followups]);

  // ── Auto-scroll ──
  useLayoutEffect(() => {
    const prev = prevScrollStateRef.current;

    if (restoringOlderHistoryRef.current || !isNearBottomRef.current) {
      prevScrollStateRef.current = {
        timelineLength: timeline.length,
        pendingFollowups: pendingFollowups.length,
        loading,
        streamingContent: streamingContent ?? "",
        loadingStatus,
      };
      return;
    }

    const hasExistingTimeline = prev.timelineLength > 0;
    const timelineGrew = timeline.length > prev.timelineLength;
    const pendingGrew = pendingFollowups.length > prev.pendingFollowups;
    const startedLoading = loading && !prev.loading;
    const onlyStreamingChanged =
      !timelineGrew &&
      !pendingGrew &&
      loading === prev.loading &&
      ((streamingContent ?? "") !== prev.streamingContent || loadingStatus !== prev.loadingStatus);

    const behavior: ScrollBehavior =
      !hasExistingTimeline || onlyStreamingChanged
        ? "auto"
        : timelineGrew || pendingGrew || startedLoading
          ? "smooth"
          : "auto";

    const frame = requestAnimationFrame(() => {
      scrollToBottom(behavior);
    });
    setShowScrollBtn(false);
    prevScrollStateRef.current = {
      timelineLength: timeline.length,
      pendingFollowups: pendingFollowups.length,
      loading,
      streamingContent: streamingContent ?? "",
      loadingStatus,
    };
    return () => cancelAnimationFrame(frame);
  }, [
    timeline.length,
    pendingFollowups.length,
    loading,
    loadingStatus,
    scrollToBottom,
    streamingContent,
  ]);

  // ── Render ──

  return (
    <div className={styles.panel}>
      <div className={`${styles.header} ${compactHeader ? styles.headerCompact : ""}`}>
        {!compactHeader ? (
          <div className={styles.headerInfo}>
            <div className={styles.headerNameRow}>
              <div className={styles.headerName}>{bot}</div>
              {botProvider && (
                <span
                  className={styles.providerBadge}
                  title={botModel || undefined}
                  aria-label={
                    botModel
                      ? `Provider: ${botProvider}, model: ${botModel}`
                      : `Provider: ${botProvider}`
                  }
                >
                  {botProvider.charAt(0).toUpperCase() + botProvider.slice(1)}
                </span>
              )}
            </div>
            {botDescription && <div className={styles.headerDescription}>{botDescription}</div>}
          </div>
        ) : null}
        {bots && bots.length > 0 && onSelectBot ? (
          <div className={styles.botSwitcher} aria-label="Chat bots">
            {bots.map((entry) => {
              const isActive = entry.name === bot;
              const count = unread?.[entry.name] || 0;
              return (
                <button
                  key={entry.name}
                  className={`${styles.botChip} ${isActive ? styles.botChipActive : ""}`}
                  onClick={() => onSelectBot(entry.name)}
                  aria-label={`Open bot ${entry.name}`}
                >
                  <span className={styles.botChipName}>{entry.name}</span>
                  {count > 0 && !isActive ? (
                    <span className={styles.botChipBadge}>{count}</span>
                  ) : null}
                </button>
              );
            })}
          </div>
        ) : null}
        {onWorkersToggle && (
          <div className={styles.headerActions}>
            <button className={styles.workersBtn} onClick={onWorkersToggle}>
              {workerCount ? `${workerCount} worker${workerCount !== 1 ? "s" : ""}` : "No workers"}
            </button>
          </div>
        )}
      </div>

      {RenderMessageList ? (
        <RenderMessageList
          messages={messages}
          bot={bot}
          loading={loading}
          streamingContent={streamingContent}
          loadingStatus={loadingStatus}
          onCancel={onCancel}
        />
      ) : (
        <div className={styles.messagesWrap}>
          <div className={styles.messages} onScroll={handleMessagesScroll} ref={messagesRef}>
            {loadingOlderHistory && messages.length > 0 && (
              <div className={styles.empty}>Loading older messages...</div>
            )}
            {messagesLoading && messages.length === 0 && (
              <div className={styles.empty}>Loading...</div>
            )}
            {!messagesLoading && messages.length === 0 && !loading && (
              <div className={styles.empty}>Start a conversation with {bot}</div>
            )}
            {timeline.map((item) =>
              item.kind === "followup" ? (
                <FollowupCard
                  key={`followup-${item.followup.id}`}
                  followup={item.followup}
                  workspace={workspace ?? ""}
                  inline
                />
              ) : RenderMessage ? (
                <RenderMessage key={item.msg.id} message={item.msg} bot={bot} />
              ) : (
                <div
                  key={item.msg.id}
                  className={`${styles.msg} ${item.msg.role === "user" ? styles.user : ""}`}
                >
                  <div className={styles.meta}>
                    <strong>{item.msg.role === "user" ? "You" : bot}</strong>
                    {" · "}
                    {formatTime(item.msg.created_at)}
                  </div>
                  {renderAttachments(item.msg.attachments)}
                  <div className={styles.text}>
                    {item.msg.role === "assistant" ? (
                      <Markdown remarkPlugins={[remarkGfm]}>{item.msg.content}</Markdown>
                    ) : (
                      item.msg.content
                    )}
                  </div>
                </div>
              ),
            )}
            {loading && (
              <div className={styles.msg}>
                <div className={styles.meta}>
                  <strong>{bot}</strong>
                  {onCancel && (
                    <button className={styles.cancelBtn} onClick={onCancel}>
                      Stop
                    </button>
                  )}
                </div>
                {streamingContent ? (
                  <>
                    <div className={styles.text}>
                      <Markdown remarkPlugins={[remarkGfm]}>{streamingContent}</Markdown>
                    </div>
                    <div className={styles.streamingIndicator}>
                      <span className={styles.thinkingDots}>
                        <span />
                        <span />
                        <span />
                      </span>
                      {loadingStatus && (
                        <span className={styles.thinkingStatus}>{loadingStatus}</span>
                      )}
                    </div>
                  </>
                ) : (
                  <div className={styles.thinking}>
                    <span className={styles.thinkingDots}>
                      <span />
                      <span />
                      <span />
                    </span>
                    {loadingStatus && (
                      <span className={styles.thinkingStatus}>{loadingStatus}</span>
                    )}
                  </div>
                )}
              </div>
            )}
            {workspace &&
              pendingFollowups.map((f) => (
                <FollowupCard
                  key={f.id}
                  followup={f}
                  workspace={workspace}
                  onCancelled={() => onFollowupCancelled?.()}
                />
              ))}
            <div />
          </div>
          {followups && followups.some((f) => f.status === "pending") && showScrollBtn && (
            <FollowupIndicator followup={followups.find((f) => f.status === "pending")!} />
          )}
          <button
            className={`${styles.scrollToBottom} ${showScrollBtn ? styles.scrollToBottomVisible : ""}`}
            onClick={handleScrollToBottom}
            aria-label="Scroll to bottom"
            tabIndex={showScrollBtn ? 0 : -1}
            aria-hidden={!showScrollBtn}
            disabled={!showScrollBtn}
          >
            <ChevronDown size={20} />
          </button>
        </div>
      )}

      {RenderInput ? (
        <RenderInput
          placeholder={`Message ${bot}...`}
          loading={loading}
          onSend={handleSendOrQueue}
          onCancel={onCancel}
          queueCount={messageQueue.length}
        />
      ) : (
        <ChatInput
          placeholder={`Message ${bot}...`}
          disabled={loading}
          onSend={handleSendOrQueue}
          queueCount={messageQueue.length}
        />
      )}
    </div>
  );
}
