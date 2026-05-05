import { useEffect, useRef, useState, useCallback } from 'react'
import ReactMarkdown from 'react-markdown'
import { ExternalLink, MessageSquare } from 'lucide-react'
import type { WorkerDetailV2 as WorkerDetailV2Data, WorkerV2, WorkerReview, ContextBotContext } from '../../types'
import { getWorkerV2, sendWorkerMessageV2, cancelWorkerV2, requeueWorkerV2, requestWorkerReview, listWorkerReviews } from '../../api'
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
      {/* Tests passing — only shown when explicitly confirmed true */}
      {worker.tests_passing && (
        <span className={`${styles.pill} ${styles.pillGreen}`}>
          Local tests ✓
        </span>
      )}

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
  const [reviews, setReviews] = useState<WorkerReview[]>([])
  const [reviewing, setReviewing] = useState(false)
  const eventsEndRef = useRef<HTMLDivElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const pollRef = useRef<number | null>(null)

  const loadReviews = useCallback(async () => {
    try {
      const r = await listWorkerReviews(workspace, workerId)
      setReviews(r)
    } catch {
      // silently ignore
    }
  }, [workspace, workerId])

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
    loadReviews()

    pollRef.current = window.setInterval(() => {
      load(false)
    }, 3000)

    return () => {
      if (pollRef.current !== null) {
        window.clearInterval(pollRef.current)
        pollRef.current = null
      }
    }
  }, [load, loadReviews])

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

  const handleRequestReview = async () => {
    if (reviewing) return
    setReviewing(true)
    try {
      await requestWorkerReview(workspace, workerId)
      // Re-fetch reviews after a short delay to catch fast completions
      window.setTimeout(() => {
        loadReviews()
      }, 2000)
    } catch (e) {
      console.error('review request failed', e)
    } finally {
      setReviewing(false)
    }
  }

  if (loading) {
    return (
      <div className={styles.container}>
        <div className={styles.skeletonWrapper} data-testid="loading-skeleton">
          <div className={`${styles.skeletonLine} ${styles.skeletonTitle}`} />
          <div className={`${styles.skeletonLine} ${styles.skeletonMeta}`} />
          <div className={`${styles.skeletonLine} ${styles.skeletonBody}`} />
        </div>
      </div>
    )
  }

  if (error || !data) {
    return (
      <div className={styles.container}>
        <div className={styles.errorCenter} data-testid="error-state">
          <span className={styles.errorText}>{error ?? 'Worker not found'}</span>
          <button
            className={styles.retryBtn}
            type="button"
            onClick={() => { setError(null); setLoading(true); load(true) }}
            data-testid="retry-btn"
          >
            Retry
          </button>
        </div>
      </div>
    )
  }

  const canCancel = ['running', 'waiting', 'queued'].includes(data.state)
  const canRequeue = data.state === 'failed' || data.state === 'abandoned'
  const canReview = data.state === 'waiting' && data.branch_ready

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

      {/* Reviews section */}
      {reviews.length > 0 && (
        <div className={styles.reviewsSection} data-testid="reviews-section">
          <div className={styles.reviewsSectionTitle}>Reviews</div>
          {reviews.map((review) => (
            <ReviewCard key={review.id} review={review} />
          ))}
        </div>
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

        {(canCancel || canRequeue || canReview) && (
          <div className={styles.secondaryActions}>
            {canReview && (
              <button
                className={styles.actionBtn}
                onClick={handleRequestReview}
                disabled={reviewing}
                type="button"
                data-testid="review-btn"
              >
                {reviewing ? 'Reviewing…' : 'Request Review'}
              </button>
            )}
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

// ── Review card ──────────────────────────────────────────────────────────

function verdictClass(verdict: string): string {
  switch (verdict) {
    case 'approve': return styles.verdictApprove
    case 'request_changes': return styles.verdictRequestChanges
    case 'comment': return styles.verdictComment
    default: return styles.verdictComment
  }
}

function verdictLabel(verdict: string): string {
  switch (verdict) {
    case 'approve': return 'Approved'
    case 'request_changes': return 'Changes requested'
    case 'comment': return 'Comment'
    default: return verdict
  }
}

function severityClass(severity: string): string {
  switch (severity) {
    case 'blocking': return styles.severityBlocking
    case 'suggestion': return styles.severitySuggestion
    case 'nitpick': return styles.severityNitpick
    default: return styles.severityNitpick
  }
}

function ReviewCard({ review }: { review: WorkerReview }) {
  const time = formatTime(review.created_at)
  return (
    <div className={styles.reviewCard} data-testid="review-card">
      <div className={styles.reviewCardHeader}>
        <span className={styles.reviewerName}>{review.reviewer}</span>
        <span className={`${styles.verdictBadge} ${verdictClass(review.verdict)}`}>
          {verdictLabel(review.verdict)}
        </span>
        <span className={styles.reviewTime}>{time}</span>
      </div>
      <p className={styles.reviewSummary}>{review.summary}</p>
      {review.issues.length > 0 && (
        <ul className={styles.issueList}>
          {review.issues.map((issue, i) => (
            <li key={i} className={styles.issueItem}>
              <span className={`${styles.severityBadge} ${severityClass(issue.severity)}`}>
                {issue.severity}
              </span>
              <span className={styles.issueFile}>{issue.file}</span>
              <span className={styles.issueDesc}>{issue.description}</span>
            </li>
          ))}
        </ul>
      )}
      {review.worker_message && (
        <div className={styles.workerMessageBox}>
          <div className={styles.workerMessageLabel}>Sent to worker</div>
          <div className={styles.workerMessageText}>{review.worker_message}</div>
        </div>
      )}
    </div>
  )
}
