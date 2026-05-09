import { useState, useEffect } from "react";
import { getBots, getUnread, connectWebSocket } from "@apiari/api";
import { ChatLauncher } from "../src/ChatLauncher/ChatLauncher";
import type { ChatTheme } from "../src/ChatLauncher/chatTheme";
import {
  triggerIncomingMessage,
  triggerStreamingResponse,
  triggerToolUse,
  triggerIdle,
  resetMockStore,
  MOCK_BOTS,
} from "./mockServer";

type Position = "right" | "left";

const WORKSPACE = "demo";

const THEMES: Record<string, ChatTheme> = {
  dark: {},
  gold: { accent: "#f5c542", bg: "#1a1600", bgWindow: "#0f0e00", border: "#2a2400" },
  blue: { accent: "#4a9eff", bg: "#0d1a2e", bgWindow: "#091422", border: "#1a3050" },
  purple: { accent: "#b56ef0", bg: "#160d22", bgWindow: "#0e0818", border: "#2a1550" },
  light: {
    accent: "#6366f1",
    bg: "#f5f5f5",
    bgWindow: "#ffffff",
    bgInput: "#f0f0f0",
    border: "#e0e0e0",
    text: "#555555",
    textStrong: "#111111",
    textFaint: "#aaaaaa",
  },
};

const params = new URLSearchParams(window.location.search);
const LIVE_MODE = params.get("mock") === "false";

// ── Tiny shared style helpers ───────────────────────────────────────────────

function Btn({
  label,
  color = "#222",
  textColor = "#aaa",
  onClick,
}: {
  label: string;
  color?: string;
  textColor?: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      style={{
        padding: "6px 12px",
        borderRadius: 6,
        border: "1px solid #2a2a2a",
        background: color,
        color: textColor,
        fontSize: 12,
        cursor: "pointer",
        whiteSpace: "nowrap",
        fontFamily: "inherit",
        transition: "opacity 0.1s",
      }}
      onMouseEnter={(e) => ((e.target as HTMLElement).style.opacity = "0.8")}
      onMouseLeave={(e) => ((e.target as HTMLElement).style.opacity = "1")}
    >
      {label}
    </button>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <div
        style={{
          fontSize: 10,
          fontWeight: 600,
          letterSpacing: "0.08em",
          textTransform: "uppercase",
          color: "#444",
        }}
      >
        {title}
      </div>
      <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>{children}</div>
    </div>
  );
}

// ── App ─────────────────────────────────────────────────────────────────────

function DebugBar({ launcherKey }: { launcherKey: number }) {
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [botCount, setBotCount] = useState(0);

  useEffect(() => {
    async function syncUnread() {
      try {
        setUnread(await getUnread(WORKSPACE));
      } catch {
        // ignore fetch errors in demo
      }
    }

    // initial fetch
    getBots(WORKSPACE)
      .then((b) => setBotCount(b.length))
      .catch(() => {});
    syncUnread();

    // live updates via WebSocket — always re-read the mock store so the
    // debug bar reflects actual unread state instead of inferring it locally.
    const ws = connectWebSocket((event) => {
      if (event.workspace !== WORKSPACE) return;
      if (event.type === "unread_sync" || event.type === "message" || event.type === "seen") {
        syncUnread();
      }
    });
    return () => ws.close();
  }, [launcherKey]);

  const total = Object.values(unread).reduce((a, b) => a + b, 0);

  return (
    <div
      style={{
        position: "fixed",
        top: 0,
        left: 0,
        right: 0,
        background: "#000",
        borderBottom: "1px solid #333",
        padding: "6px 16px",
        fontSize: 11,
        fontFamily: "monospace",
        color: "#666",
        zIndex: 1000,
        display: "flex",
        flexWrap: "wrap",
        gap: 20,
      }}
    >
      <span style={{ color: "#444" }}>DEBUG</span>
      <span>
        bots: <strong style={{ color: "#aaa" }}>{botCount}</strong>
      </span>
      <span>
        totalUnread: <strong style={{ color: total > 0 ? "#e85555" : "#aaa" }}>{total}</strong>
      </span>
      <span>
        per-bot: <strong style={{ color: "#aaa" }}>{JSON.stringify(unread)}</strong>
      </span>
    </div>
  );
}

export default function App() {
  const [position, setPosition] = useState<Position>("right");
  const [themeName, setThemeName] = useState("dark");
  const [launcherKey, setLauncherKey] = useState(0);

  function reset() {
    resetMockStore();
    setLauncherKey((k) => k + 1);
  }

  return (
    <div
      style={{
        minHeight: "100vh",
        background: "#0d0d0d",
        display: "flex",
        flexDirection: "column",
        fontFamily: "system-ui, -apple-system, sans-serif",
        color: "#aaa",
      }}
    >
      <DebugBar launcherKey={launcherKey} />

      {/* Header */}
      <div
        style={{
          padding: "14px 24px",
          marginTop: 29,
          borderBottom: "1px solid #1a1a1a",
          display: "flex",
          alignItems: "center",
          gap: 16,
          flexWrap: "wrap",
        }}
      >
        <div>
          <span style={{ color: "#eee", fontWeight: 600, fontSize: 14 }}>@apiari/chat</span>
          <span style={{ color: "#333", fontSize: 12, marginLeft: 10 }}>
            {LIVE_MODE ? "live · daemon :4200" : "mock · no daemon needed"}
          </span>
        </div>
        <div style={{ marginLeft: "auto", display: "flex", gap: 6, flexWrap: "wrap" }}>
          {/* Theme */}
          {Object.keys(THEMES).map((name) => (
            <button
              key={name}
              onClick={() => setThemeName(name)}
              style={{
                padding: "4px 10px",
                borderRadius: 5,
                border: `1px solid ${themeName === name ? "#444" : "#1e1e1e"}`,
                background: themeName === name ? "#1e1e1e" : "transparent",
                color: themeName === name ? "#ccc" : "#444",
                fontSize: 11,
                cursor: "pointer",
                fontFamily: "inherit",
              }}
            >
              {name}
            </button>
          ))}
          <div style={{ width: 1, background: "#1e1e1e", margin: "0 2px" }} />
          {/* Position */}
          {(["right", "left"] as Position[]).map((p) => (
            <button
              key={p}
              onClick={() => setPosition(p)}
              style={{
                padding: "4px 10px",
                borderRadius: 5,
                border: `1px solid ${position === p ? "#444" : "#1e1e1e"}`,
                background: position === p ? "#1e1e1e" : "transparent",
                color: position === p ? "#ccc" : "#444",
                fontSize: 11,
                cursor: "pointer",
                fontFamily: "inherit",
              }}
            >
              {p}
            </button>
          ))}
          <div style={{ width: 1, background: "#1e1e1e", margin: "0 2px" }} />
          <a
            href={LIVE_MODE ? "?" : "?mock=false"}
            style={{
              padding: "4px 10px",
              borderRadius: 5,
              border: "1px solid #1e1e1e",
              color: "#444",
              fontSize: 11,
              textDecoration: "none",
            }}
          >
            {LIVE_MODE ? "use mock" : "use live"}
          </a>
        </div>
      </div>

      {/* Main content */}
      <div style={{ flex: 1, display: "flex", gap: 0, minHeight: 0 }}>
        {/* Controls sidebar */}
        <div
          style={{
            width: 260,
            borderRight: "1px solid #1a1a1a",
            padding: "20px 16px",
            display: "flex",
            flexDirection: "column",
            gap: 24,
            flexShrink: 0,
            overflowY: "auto",
          }}
        >
          <div style={{ fontSize: 11, color: "#333", lineHeight: 1.6 }}>
            Use these controls to simulate server-side events and test all UI states. The launcher
            button is in the {position === "right" ? "bottom-right" : "bottom-left"} corner.
          </div>

          {/* Reset */}
          <Section title="Reset">
            <Btn label="Reset everything" color="#1a1a0a" textColor="#f5c542" onClick={reset} />
          </Section>

          {/* Unread: trigger incoming messages */}
          <Section title="Trigger unread">
            {MOCK_BOTS.map((bot) => (
              <Btn
                key={bot.name}
                label={`+ msg → ${bot.name}`}
                color="#1a0a0a"
                textColor="#e85555"
                onClick={() => triggerIncomingMessage(WORKSPACE, bot.name)}
              />
            ))}
          </Section>

          {/* Streaming: per bot */}
          <Section title="Trigger streaming response">
            {MOCK_BOTS.map((bot) => (
              <Btn
                key={bot.name}
                label={`Stream → ${bot.name}`}
                color="#0a0f1a"
                textColor="#4a9eff"
                onClick={() => triggerStreamingResponse(WORKSPACE, bot.name)}
              />
            ))}
          </Section>

          {/* Tool use state */}
          <Section title="Trigger tool use (thinking)">
            {MOCK_BOTS.map((bot) => (
              <Btn
                key={bot.name}
                label={`Tool → ${bot.name}`}
                color="#110a1a"
                textColor="#b56ef0"
                onClick={() => triggerToolUse(WORKSPACE, bot.name)}
              />
            ))}
          </Section>

          {/* Reset idle */}
          <Section title="Reset to idle">
            {MOCK_BOTS.map((bot) => (
              <Btn
                key={bot.name}
                label={`Idle → ${bot.name}`}
                onClick={() => triggerIdle(WORKSPACE, bot.name)}
              />
            ))}
          </Section>

          {/* State legend */}
          <div
            style={{
              marginTop: "auto",
              padding: "12px",
              background: "#111",
              borderRadius: 8,
              border: "1px solid #1a1a1a",
              fontSize: 11,
              lineHeight: 1.8,
              color: "#444",
            }}
          >
            <div style={{ color: "#333", fontWeight: 600, marginBottom: 6 }}>Button states</div>
            <div>◯ Default — no open chats</div>
            <div>🟢 Open — live chats, no unread</div>
            <div>🔴 Unread — new messages waiting</div>
            <div>✕ Popover open</div>
          </div>
        </div>

        {/* Canvas */}
        <div
          style={{
            flex: 1,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            flexDirection: "column",
            gap: 12,
            padding: 40,
            color: "#222",
            fontSize: 13,
            textAlign: "center",
          }}
        >
          <div style={{ color: "#1e1e1e", maxWidth: 280, lineHeight: 1.7 }}>
            Open a chat from the launcher, then use the controls on the left to trigger events while
            windows are open or minimized.
          </div>
          {Object.keys(THEMES[themeName]).length > 0 && (
            <div
              style={{
                display: "flex",
                gap: 5,
                flexWrap: "wrap",
                justifyContent: "center",
                maxWidth: 360,
              }}
            >
              {Object.entries(THEMES[themeName]).map(([k, v]) => (
                <code
                  key={k}
                  style={{
                    padding: "2px 7px",
                    borderRadius: 4,
                    background: "#111",
                    border: "1px solid #1a1a1a",
                    fontSize: 10,
                    color: "#333",
                  }}
                >
                  {k}: {v}
                </code>
              ))}
            </div>
          )}
        </div>
      </div>

      <ChatLauncher
        key={launcherKey}
        workspace={WORKSPACE}
        position={position}
        theme={THEMES[themeName]}
      />
    </div>
  );
}
