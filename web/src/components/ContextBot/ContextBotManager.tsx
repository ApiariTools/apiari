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
  if (sessions.length === 0) return null

  return (
    <div className={styles.manager} data-testid="context-bot-manager">
      {sessions.map((session) => (
        <ContextBotPanel
          key={session.id}
          session={session}
          onSend={onSend}
          onChangeModel={onChangeModel}
          onMinimize={onMinimize}
          onClose={onClose}
        />
      ))}
    </div>
  )
}
