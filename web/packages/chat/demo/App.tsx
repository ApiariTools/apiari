import { useState, useEffect, useCallback } from "react";
import { ChatPanel } from "../src/ChatPanel";
import { getConversations, sendMessage, getBotStatus, connectWebSocket } from "@apiari/api";
import type { Message, Bot } from "@apiari/types";

const params = new URLSearchParams(window.location.search);
const WORKSPACE = params.get("ws") || "default";
const BOT = params.get("bot") || "Main";

export default function App() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [loading, setLoading] = useState(false);
  const [streamingContent, setStreamingContent] = useState("");
  const [loadingStatus, setLoadingStatus] = useState<string | undefined>();

  useEffect(() => {
    getConversations(WORKSPACE, BOT).then(setMessages).catch(() => {});
  }, []);

  useEffect(() => {
    const ws = connectWebSocket((event) => {
      if (event.workspace !== WORKSPACE || event.bot !== BOT) return;
      if (event.type === "message") {
        setMessages((prev) => {
          const msg = event as unknown as Message;
          if (prev.find((m) => m.id === msg.id)) return prev;
          return [...prev, msg];
        });
        setLoading(false);
        setStreamingContent("");
        setLoadingStatus(undefined);
      }
      if (event.type === "bot_status") {
        const status = event.status as string;
        setLoading(status !== "idle");
        setStreamingContent((event.streaming_content as string) || "");
        setLoadingStatus((event.tool_name as string) || undefined);
      }
    });
    return () => ws.close();
  }, []);

  const handleSend = useCallback(async (text: string) => {
    setLoading(true);
    try {
      await sendMessage(WORKSPACE, BOT, text);
    } catch {
      setLoading(false);
    }
  }, []);

  const handleCancel = useCallback(async () => {
    const { cancelBot } = await import("@apiari/api");
    await cancelBot(WORKSPACE, BOT).catch(() => {});
    setLoading(false);
  }, []);

  const bot: Bot = { name: BOT, watch: [] };

  return (
    <div style={{ height: "100vh", display: "flex", flexDirection: "column" }}>
      <div style={{ padding: "8px 16px", background: "var(--bg-card)", borderBottom: "1px solid var(--border)", fontSize: 13, color: "var(--text-faint)" }}>
        <strong style={{ color: "var(--text)" }}>@apiari/chat demo</strong>
        {" · "}workspace: <code>{WORKSPACE}</code>
        {" · "}bot: <code>{BOT}</code>
        {" · "}
        <a href="?">reset</a>
      </div>
      <div style={{ flex: 1, minHeight: 0 }}>
        <ChatPanel
          bot={BOT}
          bots={[bot]}
          messages={messages}
          messagesLoading={false}
          loading={loading}
          loadingStatus={loadingStatus}
          streamingContent={streamingContent}
          onSend={handleSend}
          onCancel={handleCancel}
          workspace={WORKSPACE}
        />
      </div>
    </div>
  );
}
