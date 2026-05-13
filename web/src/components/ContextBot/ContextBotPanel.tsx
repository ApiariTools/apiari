import { useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import { Minus } from "lucide-react";
import type { ContextBotSession } from "@apiari/types";
import { Button, Select, Input, Dots } from "@apiari/ui";
import styles from "./ContextBot.module.css";

export interface ContextBotPanelProps {
  session: ContextBotSession;
  isActive?: boolean;
  onSend: (sessionId: string, message: string) => void;
  onChangeModel: (sessionId: string, model: string) => void;
  onMinimize: (sessionId: string) => void;
  onClose: (sessionId: string) => void;
}

const MODELS = [
  { id: "claude-haiku-4-5-20251001", label: "Haiku 4.5" },
  { id: "claude-sonnet-4-6", label: "Sonnet 4.6" },
  { id: "claude-opus-4-7", label: "Opus 4.7" },
  { id: "o4-mini", label: "o4-mini (Codex)" },
  { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
];

function modelLabel(id: string): string {
  return MODELS.find((m) => m.id === id)?.label ?? id;
}

export default function ContextBotPanel({
  session,
  isActive = true,
  onSend,
  onChangeModel,
  onMinimize,
  onClose,
}: ContextBotPanelProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [confirmingEnd, setConfirmingEnd] = useState(false);
  const locked = session.messages.length > 0;

  const handleSend = () => {
    const input = inputRef.current;
    if (!input) return;
    const message = input.value.trim();
    if (!message || session.loading) return;
    onSend(session.id, message);
    input.value = "";
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      const isMobile = window.matchMedia("(hover: none)").matches;
      if (!isMobile) {
        e.preventDefault();
        handleSend();
      }
    }
  };

  return (
    <div
      className={`${styles.panel} ${session.minimized ? styles.panelMinimized : ""} ${!isActive ? styles.panelHidden : ""}`}
      data-testid="context-bot-panel"
    >
      {/* Header — tappable when minimized to re-expand */}
      <div
        className={styles.header}
        onClick={session.minimized ? () => onMinimize(session.id) : undefined}
      >
        <div className={styles.headerInfo}>
          <span className={styles.headerDot} aria-hidden="true" />
          <span className={styles.headerTitle} data-testid="panel-title">
            {session.title}
          </span>
        </div>
        <div className={styles.headerActions}>
          {confirmingEnd ? (
            <>
              <span className={styles.confirmLabel}>End chat?</span>
              <button
                className={styles.confirmNo}
                onClick={() => setConfirmingEnd(false)}
                type="button"
              >
                Cancel
              </button>
              <button
                className={styles.confirmYes}
                onClick={() => onClose(session.id)}
                type="button"
                data-testid="confirm-end-btn"
              >
                End
              </button>
            </>
          ) : (
            <>
              {locked ? (
                <span className={styles.modelBadge} title={session.model}>
                  {modelLabel(session.model)}
                </span>
              ) : (
                <Select
                  size="sm"
                  value={session.model}
                  onChange={(e) => onChangeModel(session.id, e.target.value)}
                  data-testid="model-select"
                >
                  {MODELS.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.label}
                    </option>
                  ))}
                </Select>
              )}
              <Button
                variant="icon"
                size="md"
                onClick={() => onMinimize(session.id)}
                aria-label="Minimize"
                data-testid="minimize-btn"
              >
                <Minus size={18} />
              </Button>
              <button
                className={styles.endBtn}
                onClick={() => setConfirmingEnd(true)}
                type="button"
                aria-label="End chat"
                data-testid="close-btn"
              >
                End
              </button>
            </>
          )}
        </div>
      </div>

      {/* Body — hidden when minimized */}
      {!session.minimized && (
        <>
          {/* Messages */}
          <div className={styles.messages} data-testid="messages-area">
            {session.messages.length === 0 && !session.loading && (
              <div className={styles.emptyHint}>Ask anything about what you're viewing.</div>
            )}

            {session.messages.map((msg, i) =>
              msg.role === "user" ? (
                <div key={i} className={styles.msgUser}>
                  <span className={styles.msgUserLabel}>You</span>
                  <div className={styles.msgUserContent}>{msg.content}</div>
                </div>
              ) : (
                <div key={i} className={styles.msgAssistant}>
                  <div className={styles.msgAssistantContent}>
                    <ReactMarkdown>{msg.content}</ReactMarkdown>
                  </div>
                </div>
              ),
            )}

            {session.loading && (
              <div className={styles.loadingActivity} data-testid="loading-dots">
                <Dots />
                {session.activity && (
                  <span className={styles.loadingActivityLabel}>{session.activity}</span>
                )}
              </div>
            )}
          </div>

          {/* Input bar */}
          <div className={styles.inputBar}>
            <div className={styles.inputWrap}>
              <Input
                ref={inputRef}
                type="text"
                placeholder="Ask..."
                onKeyDown={handleKeyDown}
                disabled={session.loading}
                data-testid="chat-input"
              />
            </div>
            <button
              className={styles.sendBtn}
              onClick={handleSend}
              onMouseDown={(e) => e.preventDefault()}
              disabled={session.loading}
              type="button"
              data-testid="send-btn"
            >
              Send
            </button>
          </div>
        </>
      )}
    </div>
  );
}
