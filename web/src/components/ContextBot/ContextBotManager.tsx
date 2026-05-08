import { useState, useEffect, useRef } from 'react'
import { MessageSquare, Plus } from 'lucide-react'
import type { ContextBotContext, ContextBotSession } from '../../types'
import ContextBotPanel from './ContextBotPanel'
import styles from './ContextBot.module.css'

export interface ContextBotManagerProps {
  sessions: ContextBotSession[]
  currentTarget: { context: ContextBotContext; title: string } | null
  onNewSession: (context: ContextBotContext, title: string) => void
  onSend: (sessionId: string, message: string) => void
  onChangeModel: (sessionId: string, model: string) => void
  onMinimize: (sessionId: string) => void
  onClose: (sessionId: string) => void
}

export default function ContextBotManager({
  sessions, currentTarget, onNewSession,
  onSend, onChangeModel, onMinimize, onClose,
}: ContextBotManagerProps) {
  const [activeId, setActiveId] = useState<string | null>(null)
  const [fabMenuOpen, setFabMenuOpen] = useState(false)
  const fabRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (sessions.length === 0) { setActiveId(null); return }
    const ids = new Set(sessions.map((s) => s.id))
    setActiveId((prev) => (prev && ids.has(prev) ? prev : sessions[sessions.length - 1].id))
  }, [sessions.map((s) => s.id).join(',')])

  useEffect(() => {
    if (!fabMenuOpen) return
    const handler = (e: MouseEvent) => {
      if (fabRef.current && !fabRef.current.contains(e.target as Node)) setFabMenuOpen(false)
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [fabMenuOpen])

  const effectiveActiveId = activeId ?? sessions[sessions.length - 1]?.id ?? null
  const allMinimized = sessions.length === 0 || sessions.every((s) => s.minimized)

  const handleClose = (id: string) => {
    if (id === effectiveActiveId) {
      const remaining = sessions.filter((s) => s.id !== id)
      setActiveId(remaining.length > 0 ? remaining[remaining.length - 1].id : null)
    }
    onClose(id)
  }

  const openSession = (id: string) => {
    setActiveId(id)
    const s = sessions.find((s) => s.id === id)
    if (s?.minimized) onMinimize(id)
    setFabMenuOpen(false)
  }

  const handleNewChat = () => {
    if (currentTarget) onNewSession(currentTarget.context, currentTarget.title)
    setFabMenuOpen(false)
  }

  const handleFabClick = () => {
    // If no sessions and there's a context, start a new chat directly
    if (sessions.length === 0 && currentTarget) {
      handleNewChat()
      return
    }
    setFabMenuOpen((v) => !v)
  }

  // Mobile FAB — always rendered (handles everything on mobile)
  const fab = (
    <div ref={fabRef} className={styles.fabWrap} data-testid="context-bot-fab-wrap">
      {fabMenuOpen && (
        <div className={styles.fabMenu} data-testid="fab-menu">
          {sessions.map((s) => (
            <button key={s.id} className={styles.fabMenuItem} onClick={() => openSession(s.id)} type="button">
              <MessageSquare size={14} />
              <span>{s.title}</span>
            </button>
          ))}
          {currentTarget && (
            <button className={`${styles.fabMenuItem} ${styles.fabMenuItemNew}`} onClick={handleNewChat} type="button">
              <Plus size={14} />
              <span>New chat{currentTarget ? ` — ${currentTarget.title}` : ''}</span>
            </button>
          )}
        </div>
      )}
      <button
        className={styles.fab}
        onClick={handleFabClick}
        type="button"
        aria-label={sessions.length === 0 ? 'Start chat' : `Chats (${sessions.length})`}
        data-testid="context-bot-fab"
      >
        <MessageSquare size={22} />
        {sessions.length > 0 && (
          <span className={styles.fabBadge}>{sessions.length}</span>
        )}
      </button>
    </div>
  )

  // No open sessions — show FAB only (desktop: nothing; mobile: FAB)
  if (allMinimized) {
    return fab
  }

  return (
    <>
      {/* Mobile FAB overlays the open panel so user can switch/new */}
      <div className={styles.fabMobileOnly}>{fab}</div>

      <div className={styles.manager} data-testid="context-bot-manager">
        {sessions.map((session) => (
          <ContextBotPanel
            key={session.id}
            session={session}
            isActive={session.id === effectiveActiveId}
            onSend={onSend}
            onChangeModel={onChangeModel}
            onMinimize={(id) => {
              const next = sessions.find(s => s.id !== id && !s.minimized)
              if (next) setActiveId(next.id)
              onMinimize(id)
            }}
            onClose={handleClose}
          />
        ))}
      </div>
    </>
  )
}
