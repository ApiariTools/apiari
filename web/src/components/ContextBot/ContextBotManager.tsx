import { useState, useEffect } from 'react'
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

  const handleClose = (id: string) => {
    // If closing the active session, switch to the most recent remaining one
    if (id === effectiveActiveId) {
      const remaining = sessions.filter((s) => s.id !== id)
      setActiveId(remaining.length > 0 ? remaining[remaining.length - 1].id : null)
    }
    onClose(id)
  }

  return (
    <div className={styles.manager} data-testid="context-bot-manager">
      {/* Session tab strip — shown on mobile when multiple sessions open */}
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
