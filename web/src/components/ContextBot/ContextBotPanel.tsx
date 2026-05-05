import { useRef } from 'react'
import ReactMarkdown from 'react-markdown'
import { Minus, X } from 'lucide-react'
import type { ContextBotSession } from '../../types'
import styles from './ContextBot.module.css'

export interface ContextBotPanelProps {
  session: ContextBotSession
  onSend: (sessionId: string, message: string) => void
  onMinimize: (sessionId: string) => void
  onClose: (sessionId: string) => void
}

export default function ContextBotPanel({ session, onSend, onMinimize, onClose }: ContextBotPanelProps) {
  const inputRef = useRef<HTMLInputElement>(null)

  const handleSend = () => {
    const input = inputRef.current
    if (!input) return
    const message = input.value.trim()
    if (!message || session.loading) return
    onSend(session.id, message)
    input.value = ''
  }

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' && !e.nativeEvent.isComposing) {
      const isMobile = window.matchMedia('(hover: none)').matches
      if (!isMobile) {
        e.preventDefault()
        handleSend()
      }
    }
  }

  return (
    <div
      className={`${styles.panel} ${session.minimized ? styles.panelMinimized : ''}`}
      data-testid="context-bot-panel"
    >
      {/* Header */}
      <div className={styles.header}>
        <div className={styles.headerInfo}>
          <span className={styles.headerDot} aria-hidden="true" />
          <span className={styles.headerTitle} data-testid="panel-title">{session.title}</span>
        </div>
        <div className={styles.headerActions}>
          <button
            className={styles.iconBtn}
            onClick={() => onMinimize(session.id)}
            type="button"
            aria-label="Minimize"
            data-testid="minimize-btn"
          >
            <Minus size={14} />
          </button>
          <button
            className={styles.iconBtn}
            onClick={() => onClose(session.id)}
            type="button"
            aria-label="Close"
            data-testid="close-btn"
          >
            <X size={14} />
          </button>
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
              msg.role === 'user' ? (
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
              )
            )}

            {session.loading && (
              <div className={styles.loadingDots} data-testid="loading-dots">
                <span />
                <span />
                <span />
              </div>
            )}
          </div>

          {/* Input bar */}
          <div className={styles.inputBar}>
            <input
              ref={inputRef}
              className={styles.input}
              type="text"
              placeholder="Ask..."
              onKeyDown={handleKeyDown}
              disabled={session.loading}
              data-testid="chat-input"
            />
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
  )
}
