import { useState, useEffect } from 'react'
import { MessageSquare } from 'lucide-react'
import type { ContextBotSession } from '../../types'
import ContextBotPanel from './ContextBotPanel'
import styles from './ContextBot.module.css'

export interface ContextBotManagerProps {
  sessions: ContextBotSession[]
  onSend: (sessionId: string, message: string) => void
  onChangeModel: (sessionId: string, model: string) => void
  onMinimize: (sessionId: string) => void
  onClose: (sessionId: string) => void
}

export default function ContextBotManager({ sessions, onSend, onChangeModel, onMinimize, onClose }: ContextBotManagerProps) {
  const [activeId, setActiveId] = useState<string | null>(null)

  // When a new session is added, make it active
  useEffect(() => {
    if (sessions.length === 0) {
      setActiveId(null)
      return
    }
    const ids = new Set(sessions.map((s) => s.id))
    setActiveId((prev) => (prev && ids.has(prev) ? prev : sessions[sessions.length - 1].id))
  }, [sessions.map((s) => s.id).join(',')])

  if (sessions.length === 0) return null

  const effectiveActiveId = activeId ?? sessions[sessions.length - 1].id
  const allMinimized = sessions.every((s) => s.minimized)

  const handleClose = (id: string) => {
    if (id === effectiveActiveId) {
      const remaining = sessions.filter((s) => s.id !== id)
      setActiveId(remaining.length > 0 ? remaining[remaining.length - 1].id : null)
    }
    onClose(id)
  }

  const handleFabClick = () => {
    // Un-minimize the active session (or most recent)
    onMinimize(effectiveActiveId)
  }

  // On mobile: when all sessions are minimized, show a FAB bubble instead of
  // keeping the full-screen overlay alive
  if (allMinimized) {
    return (
      <button
        className={styles.fab}
        onClick={handleFabClick}
        type="button"
        aria-label={`Open chat (${sessions.length})`}
        data-testid="context-bot-fab"
      >
        <MessageSquare size={22} />
        {sessions.length > 1 && (
          <span className={styles.fabBadge}>{sessions.length}</span>
        )}
      </button>
    )
  }

  return (
    <div className={styles.manager} data-testid="context-bot-manager">
      {/* Session tab strip — shown when multiple sessions open */}
      {sessions.length > 1 && (
        <div className={styles.sessionTabs} data-testid="session-tabs">
          {sessions.map((s) => (
            <button
              key={s.id}
              className={`${styles.sessionTab} ${s.id === effectiveActiveId ? styles.sessionTabActive : ''}`}
              onClick={() => setActiveId(s.id)}
              type="button"
            >
              {s.title}
            </button>
          ))}
        </div>
      )}
      {sessions.map((session) => (
        <ContextBotPanel
          key={session.id}
          session={session}
          isActive={session.id === effectiveActiveId}
          onSend={onSend}
          onChangeModel={onChangeModel}
          onMinimize={onMinimize}
          onClose={handleClose}
        />
      ))}
    </div>
  )
}
