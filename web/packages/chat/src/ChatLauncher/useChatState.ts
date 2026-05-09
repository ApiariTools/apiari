import { useState, useEffect, useCallback, useRef } from "react";
import type { Bot, Message } from "@apiari/types";
import {
  getBots,
  getConversations,
  sendMessage,
  cancelBot,
  connectWebSocket,
  getUnread,
  markSeen,
} from "@apiari/api";

export interface OpenWindow {
  bot: string;
  minimized: boolean;
}

export interface BotChatState {
  messages: Message[];
  loading: boolean;
  streamingContent: string;
  loadingStatus?: string;
}

export function useChatState(workspace: string) {
  const [bots, setBots] = useState<Bot[]>([]);
  const [openWindows, setOpenWindows] = useState<OpenWindow[]>([]);
  const [botStates, setBotStates] = useState<Record<string, BotChatState>>({});
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [showBotList, setShowBotList] = useState(false);
  const loadedBots = useRef<Set<string>>(new Set());

  useEffect(() => {
    getBots(workspace)
      .then(setBots)
      .catch(() => {});
    getUnread(workspace)
      .then(setUnread)
      .catch(() => {});
  }, [workspace]);

  useEffect(() => {
    const ws = connectWebSocket((event) => {
      if (event.workspace !== workspace) return;
      const bot = event.bot as string;

      if (event.type === "message") {
        const msg = event as unknown as Message;
        setBotStates((prev) => {
          const cur = prev[bot];
          if (!cur) return prev;
          if (cur.messages.find((m) => m.id === msg.id)) return prev;
          return {
            ...prev,
            [bot]: {
              ...cur,
              messages: [...cur.messages, msg],
              loading: false,
              streamingContent: "",
            },
          };
        });
        // update unread for windows that are minimized or closed
        setOpenWindows((wins) => {
          const win = wins.find((w) => w.bot === bot);
          if (!win || win.minimized) {
            setUnread((prev) => ({ ...prev, [bot]: (prev[bot] ?? 0) + 1 }));
          }
          return wins;
        });
      }

      if (event.type === "bot_status") {
        const status = event.status as string;
        setBotStates((prev) => {
          const cur = prev[bot];
          if (!cur) return prev;
          return {
            ...prev,
            [bot]: {
              ...cur,
              loading: status !== "idle",
              streamingContent: (event.streaming_content as string) || "",
              loadingStatus: (event.tool_name as string) || undefined,
            },
          };
        });
      }
    });
    return () => ws.close();
  }, [workspace]);

  const loadBot = useCallback(
    async (botName: string) => {
      if (loadedBots.current.has(botName)) return;
      loadedBots.current.add(botName);
      setBotStates((prev) => ({
        ...prev,
        [botName]: prev[botName] ?? {
          messages: [],
          loading: false,
          streamingContent: "",
        },
      }));
      const messages = await getConversations(workspace, botName, 30).catch(() => [] as Message[]);
      setBotStates((prev) => ({
        ...prev,
        [botName]: { ...(prev[botName] ?? { loading: false, streamingContent: "" }), messages },
      }));
    },
    [workspace],
  );

  const openBot = useCallback(
    (botName: string) => {
      loadBot(botName);
      setOpenWindows((prev) => {
        if (prev.find((w) => w.bot === botName)) {
          return prev.map((w) => (w.bot === botName ? { ...w, minimized: false } : w));
        }
        return [...prev, { bot: botName, minimized: false }];
      });
      setShowBotList(false);
      markSeen(workspace, botName).catch(() => {});
      setUnread((prev) => ({ ...prev, [botName]: 0 }));
    },
    [workspace, loadBot],
  );

  const closeBot = useCallback((botName: string) => {
    setOpenWindows((prev) => prev.filter((w) => w.bot !== botName));
    loadedBots.current.delete(botName);
    setBotStates((prev) => {
      const next = { ...prev };
      delete next[botName];
      return next;
    });
  }, []);

  const minimizeBot = useCallback((botName: string) => {
    setOpenWindows((prev) => prev.map((w) => (w.bot === botName ? { ...w, minimized: true } : w)));
  }, []);

  const restoreBot = useCallback(
    (botName: string) => {
      setOpenWindows((prev) =>
        prev.map((w) => (w.bot === botName ? { ...w, minimized: false } : w)),
      );
      markSeen(workspace, botName).catch(() => {});
      setUnread((prev) => ({ ...prev, [botName]: 0 }));
    },
    [workspace],
  );

  const send = useCallback(
    async (
      botName: string,
      text: string,
      attachments?: Array<{ name: string; type: string; dataUrl: string }>,
    ) => {
      setBotStates((prev) => ({
        ...prev,
        [botName]: { ...(prev[botName] ?? { messages: [], streamingContent: "" }), loading: true },
      }));
      try {
        await sendMessage(workspace, botName, text, attachments);
      } catch {
        setBotStates((prev) => ({
          ...prev,
          [botName]: {
            ...(prev[botName] ?? { messages: [], streamingContent: "" }),
            loading: false,
          },
        }));
      }
    },
    [workspace],
  );

  const cancel = useCallback(
    async (botName: string) => {
      await cancelBot(workspace, botName).catch(() => {});
      setBotStates((prev) => ({
        ...prev,
        [botName]: { ...(prev[botName] ?? { messages: [], streamingContent: "" }), loading: false },
      }));
    },
    [workspace],
  );

  const totalUnread = Object.values(unread).reduce((a, b) => a + b, 0);

  return {
    bots,
    openWindows,
    botStates,
    unread,
    totalUnread,
    showBotList,
    setShowBotList,
    openBot,
    closeBot,
    minimizeBot,
    restoreBot,
    send,
    cancel,
  };
}
