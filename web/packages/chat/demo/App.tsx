import { useState } from "react";
import { ChatLauncher } from "../src/ChatLauncher/ChatLauncher";
import type { ChatTheme } from "../src/ChatLauncher/chatTheme";

type Position = "right" | "left";

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

export default function App() {
  const [position, setPosition] = useState<Position>("right");
  const [themeName, setThemeName] = useState("dark");

  return (
    <div
      style={{
        minHeight: "100vh",
        background: "#0d0d0d",
        display: "flex",
        flexDirection: "column",
        fontFamily: "system-ui, -apple-system, sans-serif",
      }}
    >
      {/* Header */}
      <div
        style={{
          padding: "16px 24px",
          borderBottom: "1px solid #1e1e1e",
          display: "flex",
          alignItems: "center",
          gap: 16,
          flexWrap: "wrap",
        }}
      >
        <div>
          <div style={{ color: "#eee", fontWeight: 600, fontSize: 15 }}>@apiari/chat</div>
          <div style={{ color: "#555", fontSize: 12, marginTop: 2 }}>
            {LIVE_MODE ? "live mode — connect to daemon on :4200" : "mock mode — no daemon needed"}
          </div>
        </div>

        <div style={{ marginLeft: "auto", display: "flex", gap: 8, flexWrap: "wrap" }}>
          {/* Theme picker */}
          <div style={{ display: "flex", gap: 4 }}>
            {Object.keys(THEMES).map((name) => (
              <button
                key={name}
                onClick={() => setThemeName(name)}
                style={{
                  padding: "5px 10px",
                  borderRadius: 6,
                  border: `1px solid ${themeName === name ? "#555" : "#222"}`,
                  background: themeName === name ? "#222" : "transparent",
                  color: themeName === name ? "#eee" : "#555",
                  fontSize: 12,
                  cursor: "pointer",
                  transition: "all 0.1s",
                }}
              >
                {name}
              </button>
            ))}
          </div>

          {/* Position toggle */}
          <div style={{ display: "flex", gap: 4 }}>
            {(["right", "left"] as Position[]).map((p) => (
              <button
                key={p}
                onClick={() => setPosition(p)}
                style={{
                  padding: "5px 10px",
                  borderRadius: 6,
                  border: `1px solid ${position === p ? "#555" : "#222"}`,
                  background: position === p ? "#222" : "transparent",
                  color: position === p ? "#eee" : "#555",
                  fontSize: 12,
                  cursor: "pointer",
                  transition: "all 0.1s",
                }}
              >
                {p}
              </button>
            ))}
          </div>

          {/* Live/mock toggle */}
          <a
            href={LIVE_MODE ? "?" : "?mock=false"}
            style={{
              padding: "5px 10px",
              borderRadius: 6,
              border: "1px solid #222",
              color: "#555",
              fontSize: 12,
              textDecoration: "none",
              display: "flex",
              alignItems: "center",
            }}
          >
            {LIVE_MODE ? "use mock" : "use live"}
          </a>
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
          gap: 16,
          padding: 40,
          position: "relative",
        }}
      >
        <div style={{ color: "#222", fontSize: 13, textAlign: "center", maxWidth: 340 }}>
          Click the button in the{" "}
          <strong style={{ color: "#333" }}>
            {position === "right" ? "bottom-right" : "bottom-left"}
          </strong>{" "}
          to open a chat.
          <br />
          <span style={{ color: "#1e1e1e" }}>
            {LIVE_MODE
              ? "Sends real messages to ws=demo on your local daemon."
              : "Fully mocked — 3 bots, streaming responses, unread badges."}
          </span>
        </div>

        {/* CSS var preview */}
        <div
          style={{
            display: "flex",
            gap: 6,
            flexWrap: "wrap",
            justifyContent: "center",
            maxWidth: 400,
          }}
        >
          {Object.entries(THEMES[themeName]).map(([k, v]) => (
            <div
              key={k}
              style={{
                padding: "3px 8px",
                borderRadius: 4,
                background: "#111",
                border: "1px solid #1a1a1a",
                fontSize: 11,
                color: "#444",
                fontFamily: "monospace",
              }}
            >
              {k}: <span style={{ color: "#666" }}>{v}</span>
            </div>
          ))}
          {Object.keys(THEMES[themeName]).length === 0 && (
            <div style={{ fontSize: 12, color: "#333" }}>default theme — no overrides</div>
          )}
        </div>
      </div>

      <ChatLauncher workspace="demo" position={position} theme={THEMES[themeName]} />
    </div>
  );
}
