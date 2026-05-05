import { useEffect, useRef, useState, useCallback } from 'react'
import ReactMarkdown from 'react-markdown'
import { ExternalLink, MessageSquare } from 'lucide-react'
import type { WorkerDetailV2 as WorkerDetailV2Data, WorkerV2, ContextBotContext } from '../../types'
import { getWorkerV2, sendWorkerMessageV2, cancelWorkerV2, requeueWorkerV2 } from '../../api'
import styles from './WorkerDetailV2.module.css'

export interface WorkerDetailV2Props {
  workspace: string
  workerId: string
  onClose?: () => void
  onOpenContextBot?: (context: ContextBotContext, title: string) => void
}

// ── Status badge ─────────────────────────────────────────────────────────

function statusClass(worker: WorkerV2): string {
  if (worker.is_stalled) return styles.statusStalled
  switch (worker.state) {
    case 'running': return styles.statusRunning
    case 'waiting': return styles.statusWaiting
    case 'failed': return styles.statusFailed
    case 'merged': return styles.statusMerged
    case 'abandoned': return styles.statusAbandoned
    default: return styles.statusDefault
  }
}

function StatusBadge({ worker }: { worker: WorkerV2 }) {
  return (
    <span className={`${styles.statusBadge} ${statusClass(worker)}`} data-testid="status-badge">
      <span className={styles.statusDot} />
      {worker.label}
    </span>
  )
}

// ── Property pills ───────────────────────────────────────────────────────

function Pills({ worker }: { worker: WorkerV2 }) {
  return (
    <div className={styles.pills} data-testid="property-pills">
      {/* Tests passing */}
      <span className={`${styles.pill} ${worker.tests_passing ? styles.pillGreen : styles.pillRed}`}>
        {worker.tests_passing ? 'Tests passing' : 'Tests failing'}
      </span>

      {/* Branch ready */}
      {worker.branch_ready && (
        <span className={`${styles.pill} ${styles.pillAmber}`}>
          Branch ready
        </span>
      )}

      {/* Stalled */}
      {worker.is_stalled && (
        <span className={`${styles.pill} ${styles.pillOrange}`}>
          Stalled
        </span>
      )}

      {/* Review mode */}
      <span className={styles.pill}>
        {worker.review_mode === 'pr_first' ? 'pr first' : 'local first'}
      </span>
    </div>
  )
}

// ── Event formatting ─────────────────────────────────────────────────────

function formatTime(iso: string): string {
  try {
    const d = new Date(iso)
    if (isNaN(d.getTime())) return ''
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  } catch {
    return ''
  }
}

interface EventRowProps {
  event_type: string
  content: string
  created_at: string
}

function EventRow({ event_type, content, created_at }: EventRowProps) {
  const time = formatTime(created_at)

  if (event_type === 'user_message') {
    return (
      <div className={styles.eventRow}>
        <span className={styles.eventTime}>{time}</span>
        <div className={styles.eventBody}>
          <div className={styles.eventUserLabel}>You</div>
          <div className={styles.eventUserContent}>{content}</div>
        </div>
      </div>
    )
  }

  if (event_type === 'tool_use') {
    return (
      <div className={styles.eventRow}>
        <span className={styles.eventTime}>{time}</span>
        <div className={styles.eventBody}>
          <span className={styles.eventTool}>{content}</span>
        </div>
      </div>
    )
  }

  // assistant_text (default)
  return (
    <div className={styles.eventRow}>
      <span className={styles.eventTime}>{time}</span>
      <div className={`${styles.eventBody} ${styles.eventAssistant}`}>
        <ReactMarkdown>{content}</ReactMarkdown>
      </div>
    </div>
  )
}

// ── Main component ───────────────────────────────────────────────────────

export default function WorkerDetailV2({ workspace, workerId, onClose: _onClose, onOpenContextBot }: WorkerDetailV2Props) {
  const [data, setData] = useState<WorkerDetailV2Data | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [sending, setSending] = useState(false)
  const eventsEndRef = useRef<HTMLDivElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const pollRef = useRef<number | null>(null)

  const load = useCallback(async (initial = false) => {
    try {
      const d = await getWorkerV2(workspace, workerId)
      setData(d)
      if (initial) setLoading(false)
    } catch (e) {
      if (initial) {
        setError(e instanceof Error ? e.message : 'Failed to load worker')
        setLoading(false)
      }
    }
  }, [workspace, workerId])

  // Initial load + polling
  useEffect(() => {
    setLoading(true)
    setError(null)
    load(true)

    pollRef.current = window.setInterval(() => {
      load(false)
    }, 3000)

    return () => {
      if (pollRef.current !== null) {
        window.clearInterval(pollRef.current)
        pollRef.current = null
      }
    }
  }, [load])

  // Auto-scroll to bottom when events arrive
  useEffect(() => {
    if (data?.events?.length) {
      eventsEndRef.current?.scrollIntoView({ behavior: 'smooth' })
    }
  }, [data?.events?.length])

  const handleSend = async () => {
    const textarea = textareaRef.current
    if (!textarea) return
    const message = textarea.value.trim()
    if (!message || sending) return

    setSending(true)
    try {
      await sendWorkerMessageV2(workspace, workerId, message)
      textarea.value = ''
      // Refresh immediately
      await load(false)
    } catch (e) {
      // Silently fail for now — could add toast in Phase 5
      console.error('send failed', e)
    } finally {
      setSending(false)
    }
  }

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Desktop: Enter sends; mobile: Enter inserts newline (send via button)
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      const isMobile = window.matchMedia('(hover: none)').matches
      if (!isMobile) {
        e.preventDefault()
        handleSend()
      }
    }
  }

  const handleCancel = async () => {
    try {
      await cancelWorkerV2(workspace, workerId)
      await load(false)
    } catch (e) {
      console.error('cancel failed', e)
    }
  }

  const handleRequeue = async () => {
    try {
      await requeueWorkerV2(workspace, workerId)
      await load(false)
    } catch (e) {
      console.error('requeue failed', e)
    }
  }

  if (loading) {
    return (
      <div className={styles.container}>
        <div className={styles.stateCenter}>Loading...</div>
      </div>
    )
  }

  if (error || !data) {
    return (
      <div className={styles.container}>
        <div className={styles.stateCenter}>{error ?? 'Worker not found'}</div>
      </div>
    )
  }

  const canCancel = ['running', 'waiting', 'queued'].includes(data.state)
  const canRequeue = data.state === 'failed' || data.state === 'abandoned'

  return (
    <div className={styles.container}>
      {/* Header */}
      <div className={styles.header}>
        <div className={styles.headerTop}>
          <h1 className={styles.goal}>
            {data.goal ?? data.branch ?? data.id}
          </h1>
          {onOpenContextBot && (
            <button
              className={styles.askBtn}
              type="button"
              onClick={() => {
                onOpenContextBot(
                  {
                    view: 'worker_detail',
                    entity_id: data.id,
                    entity_snapshot: {
                      state: data.state,
                      label: data.label,
                      branch_ready: data.branch_ready,
                      pr_url: data.pr_url,
                      revision_count: data.revision_count,
                      goal: data.goal,
                      is_stalled: data.is_stalled,
                      tests_passing: data.tests_passing,
                    },
                  },
                  `Viewing: ${data.goal ?? data.branch ?? data.id}`,
                )
              }}
              data-testid="ask-btn"
            >
              <MessageSquare size={13} />
              Ask
            </button>
          )}
        </div>

        {data.branch && (
          <div className={styles.branch}>{data.branch}</div>
        )}

        <div className={styles.headerMeta}>
          <StatusBadge worker={data} />

          {data.pr_url && (
            <a
              href={data.pr_url}
              target="_blank"
              rel="noopener noreferrer"
              className={styles.prLink}
            >
              PR <ExternalLink size={12} />
            </a>
          )}

          {data.revision_count > 0 && (
            <span className={styles.revisionPill}>
              Pass {data.revision_count}
            </span>
          )}
        </div>
      </div>

      {/* Property pills */}
      <Pills worker={data} />

      {/* Events thread */}
      {data.events && data.events.length > 0 ? (
        <div className={styles.events} data-testid="events-thread">
          {data.events.map((ev, i) => (
            <EventRow
              key={i}
              event_type={ev.event_type}
              content={ev.content}
              created_at={ev.created_at}
            />
          ))}
          <div ref={eventsEndRef} />
        </div>
      ) : (
        <div className={styles.eventsEmpty}>No activity yet</div>
      )}

      {/* Action bar */}
      <div className={styles.actionBar} data-testid="action-bar">
        <div className={styles.inputRow}>
          <textarea
            ref={textareaRef}
            className={styles.textarea}
            placeholder="Send a message to the worker..."
            rows={1}
            enterKeyHint="enter"
            onKeyDown={handleKeyDown}
            onChange={(e) => {
              // Auto-grow up to ~3 lines
              e.target.style.height = 'auto'
              e.target.style.height = `${Math.min(e.target.scrollHeight, 90)}px`
            }}
          />
          <button
            className={styles.sendBtn}
            onClick={handleSend}
            disabled={sending}
            type="button"
          >
            Send
          </button>
        </div>

        {(canCancel || canRequeue) && (
          <div className={styles.secondaryActions}>
            {canCancel && (
              <button
                className={styles.actionBtnDanger}
                onClick={handleCancel}
                type="button"
              >
                Cancel
              </button>
            )}
            {canRequeue && (
              <button
                className={styles.actionBtn}
                onClick={handleRequeue}
                type="button"
              >
                Retry
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  )
}
