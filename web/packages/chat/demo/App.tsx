import { ChatLauncher } from "../src/ChatLauncher/ChatLauncher";

const params = new URLSearchParams(window.location.search);
const WORKSPACE = params.get("ws") || "default";
const POSITION = (params.get("position") as "right" | "left") || "right";

export default function App() {
  return (
    <div
      style={{
        height: "100vh",
        background: "#111",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        flexDirection: "column",
        gap: 12,
        fontFamily: "system-ui, sans-serif",
        color: "#555",
        fontSize: 13,
      }}
    >
      <div style={{ color: "#aaa", fontSize: 15 }}>@apiari/chat demo</div>
      <div>
        workspace: <code style={{ color: "#f5c542" }}>{WORKSPACE}</code>
        {" · "}
        <a href="?ws=default" style={{ color: "#555" }}>
          reset
        </a>
      </div>
      <div style={{ color: "#444", fontSize: 12 }}>
        URL params: <code>?ws=WORKSPACE&amp;position=right|left</code>
      </div>

      <ChatLauncher workspace={WORKSPACE} position={POSITION} />
    </div>
  );
}
