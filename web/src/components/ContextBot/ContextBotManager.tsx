import { useState, useEffect, useRef } from 'react'
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
  const [fabMenuOpen, setFabMenuOpen] = useState(false)
  const fabRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (sessions.length === 0) { setActiveId(null); return }
    const ids = new Set(sessions.map((s) => s.id))
    setActiveId((prev) => (prev && ids.has(prev) ? prev : sessions[sessions.length - 1].id))
  }, [sessions.map((s) => s.id).join(',')])

  // Close fab menu when clicking outside
  useEffect(() => {
    if (!fabMenuOpen) return
    const handler = (e: MouseEvent) => {
      if (fabRef.current && !fabRef.current.contains(e.target as Node)) {
        setFabMenuOpen(false)
      }
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [fabMenuOpen])

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

  const openSession = (id: string) => {
    setActiveId(id)
    // Un-minimize if needed
    const s = sessions.find((s) => s.id === id)
    if (s?.minimized) onMinimize(id)
    setFabMenuOpen(false)
  }

  const handleFabClick = () => {
    if (sessions.length === 1) {
      openSession(sessions[0].id)
    } else {
      setFabMenuOpen((v) => !v)
    }
  }

  // Mobile: when all sessions are minimized, show FAB bubble
  if (allMinimized) {
    return (
      <div ref={fabRef} className={styles.fabWrap} data-testid="context-bot-fab-wrap">
        {fabMenuOpen && (
          <div className={styles.fabMenu} data-testid="fab-menu">
            {sessions.map((s) => (
              <button
                key={s.id}
                className={styles.fabMenuItem}
                onClick={() => openSession(s.id)}
                type="button"
              >
                <MessageSquare size={14} />
                <span>{s.title}</span>
              </button>
            ))}
          </div>
        )}
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
      </div>
    )
  }

  return (
    <div className={styles.manager} data-testid="context-bot-manager">
      {sessions.map((session) => (
        <ContextBotPanel
          key={session.id}
          session={session}
          isActive={session.id === effectiveActiveId}
          onSend={onSend}
          onChangeModel={onChangeModel}
          onMinimize={(id) => {
            // Switch active to another non-minimized session if available
            const next = sessions.find(s => s.id !== id && !s.minimized)
            if (next) setActiveId(next.id)
            onMinimize(id)
          }}
          onClose={handleClose}
        />
      ))}
    </div>
  )
}
