import { useCallback, useEffect, useRef, useState } from "react";
import * as api from "../api";
import type { Message } from "../types";

const INITIAL_HISTORY_LIMIT = 100;
const HISTORY_PAGE_SIZE = 100;

function mergeMessages(prev: Message[], incoming: Message): Message[] {
  const withoutMatchingTemps = incoming.id >= 0
    ? prev.filter((msg) => !(msg.id < 0
      && msg.workspace === incoming.workspace
      && msg.bot === incoming.bot
      && msg.role === incoming.role
      && msg.content === incoming.content))
    : prev;

  const existingIndex = withoutMatchingTemps.findIndex((msg) => msg.id === incoming.id);
  if (existingIndex >= 0) {
    const next = withoutMatchingTemps.slice();
    next[existingIndex] = incoming;
    return next;
  }

  return [...withoutMatchingTemps, incoming].sort((a, b) => a.id - b.id);
}

interface Props {
  workspace: string;
  remote?: string;
  bot: string;
  mode: string;
  onUnreadRefresh: () => void;
  onResearchRefresh: () => void;
  onFollowupsRefresh: () => void;
}

export function useChatModeState({
  workspace,
  remote,
  bot,
  mode,
  onUnreadRefresh,
  onResearchRefresh,
  onFollowupsRefresh,
}: Props) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [messagesLoading, setMessagesLoading] = useState(true);
  const [loading, setLoading] = useState(false);
  const [streamingContent, setStreamingContent] = useState("");
  const [loadingStatus, setLoadingStatus] = useState<string | undefined>();
  const [historyLimit, setHistoryLimit] = useState(INITIAL_HISTORY_LIMIT);
  const [hasOlderHistory, setHasOlderHistory] = useState(false);
  const [loadingOlderHistory, setLoadingOlderHistory] = useState(false);

  const lastMsgId = useRef(0);
  const nextTempId = useRef(-1);
  const loadingRef = useRef(false);
  const tabHiddenRef = useRef(document.hidden);
  const remoteRef = useRef(remote);
  const messagesRef = useRef<Message[]>([]);
  const streamingContentRef = useRef("");
  const historyLimitRef = useRef(INITIAL_HISTORY_LIMIT);
  const activeChatKeyRef = useRef("");

  useEffect(() => { remoteRef.current = remote; }, [remote]);
  useEffect(() => { messagesRef.current = messages; }, [messages]);
  useEffect(() => { streamingContentRef.current = streamingContent; }, [streamingContent]);
  useEffect(() => { loadingRef.current = loading; }, [loading]);
  useEffect(() => { historyLimitRef.current = historyLimit; }, [historyLimit]);
  useEffect(() => { activeChatKeyRef.current = `${workspace}|${remote || ""}|${bot}|${mode}`; }, [workspace, remote, bot, mode]);

  const appendLocalMessage = useCallback((role: string, content: string, attachments?: string | null) => {
    const tempId = nextTempId.current--;
    const message: Message = {
      id: tempId,
      workspace,
      bot,
      role,
      content,
      attachments: attachments ?? null,
      created_at: new Date().toISOString(),
    };
    setMessages((prev) => mergeMessages(prev, message));
    return message;
  }, [workspace, bot]);

  const appendSystemMessage = useCallback((content: string) => {
    setMessages((prev) => [
      ...prev,
      {
        id: Date.now(),
        workspace,
        bot,
        role: "system",
        content,
        attachments: null,
        created_at: new Date().toISOString(),
      },
    ]);
  }, [workspace, bot]);

  const finalizeStreamingAssistant = useCallback(() => {
    const content = streamingContentRef.current.trim();
    if (!content) return;

    const lastMessage = messagesRef.current[messagesRef.current.length - 1];
    if (
      lastMessage
      && lastMessage.workspace === workspace
      && lastMessage.bot === bot
      && lastMessage.role === "assistant"
      && lastMessage.content === content
    ) {
      return;
    }

    appendLocalMessage("assistant", content);
  }, [appendLocalMessage, workspace, bot]);

  useEffect(() => {
    const handleVisibilityChange = () => {
      tabHiddenRef.current = document.hidden;
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);
    return () => document.removeEventListener("visibilitychange", handleVisibilityChange);
  }, []);

  const fetchConversationHistory = useCallback(async (
    limit: number,
    currentWorkspace: string,
    currentBot: string,
    currentRemote?: string,
  ) => {
    const msgs = await api.getConversations(currentWorkspace, currentBot, limit, currentRemote);
    return {
      msgs,
      hasMore: msgs.length >= limit,
    };
  }, []);

  useEffect(() => {
    const wsConn = api.connectWebSocket((event) => {
      const eventRemote = (event.remote as string) || undefined;
      const isCurrentWorkspace = event.workspace === workspace && eventRemote === remote;

      if (event.type === "bot_status") {
        if (isCurrentWorkspace && event.bot === bot) {
          if (event.status === "idle") {
            finalizeStreamingAssistant();
            setLoading(false);
            setLoadingStatus(undefined);
            setStreamingContent("");
          } else {
            setLoading(true);
            setLoadingStatus(event.tool_name ? `Using ${event.tool_name}...` : "Thinking...");
            setStreamingContent(typeof event.streaming_content === "string" ? event.streaming_content : "");
          }
        }
      }

      if (event.type === "research_update" && isCurrentWorkspace) {
        onResearchRefresh();
        if (event.status === "complete") {
          appendSystemMessage(`Research complete: ${event.topic} → docs/${event.output_file}`);
        }
      }

      if (
        (event.type === "followup_created" || event.type === "followup_fired" || event.type === "followup_cancelled")
        && isCurrentWorkspace
      ) {
        onFollowupsRefresh();
      }

      if (event.type === "message") {
        if (workspace) {
          onUnreadRefresh();
        }

        if (isCurrentWorkspace && event.bot === bot) {
          const eventMessage = event as unknown as Message;
          if (typeof eventMessage.id === "number") {
            if (
              eventMessage.role === "assistant"
              && eventMessage.content === streamingContentRef.current.trim()
            ) {
              setStreamingContent("");
            }

            lastMsgId.current = Math.max(lastMsgId.current, eventMessage.id);
            setMessages((prev) => mergeMessages(prev, {
              id: eventMessage.id,
              workspace: eventMessage.workspace,
              bot: eventMessage.bot,
              role: eventMessage.role,
              content: eventMessage.content,
              attachments: eventMessage.attachments ?? null,
              created_at: eventMessage.created_at,
            }));
          }

          fetchConversationHistory(historyLimitRef.current, workspace, bot, remote).then(({ msgs, hasMore }) => {
            const latestId = msgs.length > 0 ? msgs[msgs.length - 1].id : 0;
            if (latestId >= lastMsgId.current) {
              lastMsgId.current = latestId;
              setMessages(msgs);
              setHasOlderHistory(hasMore);
            }
          }).catch(() => {});
        }
      }
    });

    return () => wsConn.close();
  }, [workspace, remote, bot, fetchConversationHistory, finalizeStreamingAssistant, onFollowupsRefresh, onResearchRefresh, onUnreadRefresh, appendSystemMessage]);

  useEffect(() => {
    if (!workspace || !bot || mode !== "chat") return;

    let cancelled = false;
    setHistoryLimit(INITIAL_HISTORY_LIMIT);
    historyLimitRef.current = INITIAL_HISTORY_LIMIT;
    setHasOlderHistory(false);
    setLoadingOlderHistory(false);
    setMessages([]);
    setMessagesLoading(true);
    setLoading(false);
    setLoadingStatus(undefined);
    setStreamingContent("");
    lastMsgId.current = 0;

    fetchConversationHistory(INITIAL_HISTORY_LIMIT, workspace, bot, remote).then(({ msgs, hasMore }) => {
      if (cancelled) return;
      setMessages(msgs);
      setMessagesLoading(false);
      setHasOlderHistory(hasMore);
      if (msgs.length > 0) lastMsgId.current = msgs[msgs.length - 1].id;
    });

    api.getBotStatus(workspace, bot, remote).then((status) => {
      if (cancelled) return;
      if (status.status !== "idle") {
        setLoading(true);
        setLoadingStatus(status.tool_name ? `Using ${status.tool_name}...` : "Thinking...");
        setStreamingContent(status.streaming_content || "");
      }
    });

    const seenTimer = setTimeout(() => {
      api.markSeen(workspace, bot, remote);
    }, 500);

    return () => {
      cancelled = true;
      clearTimeout(seenTimer);
    };
  }, [workspace, bot, remote, mode, fetchConversationHistory]);

  useEffect(() => {
    if (!workspace || !bot || mode !== "chat") return;

    const getInterval = () => {
      if (tabHiddenRef.current) return 30000;
      if (loadingRef.current) return 2000;
      return 10000;
    };

    let timer: ReturnType<typeof setTimeout>;
    let cancelled = false;

    function poll() {
      const currentRemote = remoteRef.current;
      const conversationsPromise = fetchConversationHistory(
        historyLimitRef.current,
        workspace,
        bot,
        currentRemote,
      ).then(({ msgs, hasMore }) => {
        if (cancelled) return;
        const latestId = msgs.length > 0 ? msgs[msgs.length - 1].id : 0;
        if (latestId > lastMsgId.current) {
          lastMsgId.current = latestId;
          setMessages(msgs);
        }
        setHasOlderHistory(hasMore);
      });

      const statusPromise = api.getBotStatus(workspace, bot, currentRemote).then((status) => {
        if (cancelled) return;
        if (status.status === "idle") {
          finalizeStreamingAssistant();
          setLoading(false);
          setLoadingStatus(undefined);
          setStreamingContent("");
        } else {
          setLoading(true);
          setLoadingStatus(status.tool_name ? `Using ${status.tool_name}...` : "Thinking...");
          setStreamingContent(status.streaming_content || "");
        }
      });

      Promise.all([conversationsPromise, statusPromise]).then(() => {
        if (!cancelled) {
          timer = setTimeout(poll, getInterval());
        }
      });
    }

    timer = setTimeout(poll, getInterval());
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [workspace, bot, mode, fetchConversationHistory, finalizeStreamingAssistant]);

  const loadOlderHistory = useCallback(async () => {
    if (!workspace || !bot || mode !== "chat" || messagesLoading || loadingOlderHistory || !hasOlderHistory) {
      return;
    }

    const nextLimit = historyLimitRef.current + HISTORY_PAGE_SIZE;
    const chatKey = `${workspace}|${remote || ""}|${bot}|${mode}`;
    setLoadingOlderHistory(true);

    try {
      const { msgs, hasMore } = await fetchConversationHistory(nextLimit, workspace, bot, remote);
      if (activeChatKeyRef.current !== chatKey) return;
      historyLimitRef.current = nextLimit;
      setHistoryLimit(nextLimit);
      setMessages(msgs);
      setHasOlderHistory(hasMore);
      if (msgs.length > 0) {
        lastMsgId.current = Math.max(lastMsgId.current, msgs[msgs.length - 1].id);
      }
    } finally {
      if (activeChatKeyRef.current === chatKey) {
        setLoadingOlderHistory(false);
      }
    }
  }, [workspace, bot, mode, messagesLoading, loadingOlderHistory, hasOlderHistory, fetchConversationHistory, remote]);

  const beginUserSend = useCallback(() => {
    setMessagesLoading(false);
    setLoading(true);
    setLoadingStatus("Thinking...");
  }, []);

  return {
    messages,
    setMessages,
    messagesLoading,
    loading,
    setLoading,
    streamingContent,
    loadingStatus,
    setLoadingStatus,
    hasOlderHistory,
    loadingOlderHistory,
    loadOlderHistory,
    appendLocalMessage,
    appendSystemMessage,
    beginUserSend,
  };
}
