import { useEffect, useRef, useState, useCallback } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { ExternalLink, MessageSquare } from 'lucide-react'
import type { WorkerDetailV2 as WorkerDetailV2Data, WorkerV2, WorkerReview, WorkerBrief, ContextBotContext, WorkerEvent } from '../../types'
import { getWorkerV2, sendWorkerMessageV2, cancelWorkerV2, requeueWorkerV2, requestWorkerReview, listWorkerReviews } from '../../api'
import styles from './WorkerDetailV2.module.css'

export interface WorkerDetailV2Props {
  workspace: string
  workerId: string
  onClose?: () => void
  onOpenContextBot?: (context: ContextBotContext, title: string) => void
}

type Tab = 'timeline' | 'reviews' | 'brief'

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
  const isRunning = !worker.is_stalled && worker.state === 'running'
  return (
    <span className={`${styles.statusBadge} ${statusClass(worker)}`} data-testid="status-badge">
      <span className={`${styles.statusDot} ${isRunning ? styles.statusDotRunning : ''}`} />
      {worker.label}
    </span>
  )
}

// ── Property pills ───────────────────────────────────────────────────────

function Pills({ worker }: { worker: WorkerV2 }) {
  return (
    <div className={styles.pills} data-testid="property-pills">
      {worker.tests_passing && (
        <span className={`${styles.pill} ${styles.pillGreen}`}>
          Local tests ✓
        </span>
      )}
      {worker.branch_ready && (
        <span className={`${styles.pill} ${styles.pillAmber}`}>
          Branch ready
        </span>
      )}
      {worker.is_stalled && (
        <span className={`${styles.pill} ${styles.pillOrange}`}>
          Stalled
        </span>
      )}
      <span className={styles.pill}>
        {worker.review_mode === 'pr_first' ? 'pr first' : 'local first'}
      </span>
    </div>
  )
}

// ── Time formatting ──────────────────────────────────────────────────────

function formatTime(iso: string): string {
  try {
    const d = new Date(iso)
    if (isNaN(d.getTime())) return ''
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  } catch {
    return ''
  }
}

function formatRelative(iso: string): string {
  try {
    const d = new Date(iso)
    if (isNaN(d.getTime())) return ''
    const diffMs = Date.now() - d.getTime()
    const diffMins = Math.floor(diffMs / 60000)
    if (diffMins < 1) return 'just now'
    if (diffMins < 60) return `${diffMins} min ago`
    const diffHours = Math.floor(diffMins / 60)
    if (diffHours < 24) return `${diffHours}h ago`
    return `${Math.floor(diffHours / 24)}d ago`
  } catch {
    return ''
  }
}

// ── State divider label ──────────────────────────────────────────────────

function stateDividerLabel(state: string, hasReviews = false): string {
  switch (state) {
    case 'running': return 'Worker running'
    case 'waiting': return hasReviews ? 'Reviewed' : 'Waiting for review'
    case 'failed': return 'Worker failed'
    case 'merged': return 'Merged'
    case 'abandoned': return 'Abandoned'
    default: return state
  }
}

// ── Event helpers ─────────────────────────────────────────────────────────

function mergeConsecutiveText(events: WorkerEvent[]): WorkerEvent[] {
  const merged: WorkerEvent[] = []
  for (const evt of events) {
    const last = merged[merged.length - 1]
    if (evt.event_type === 'assistant_text' && last?.event_type === 'assistant_text') {
      last.content += evt.content
    } else {
      merged.push({ ...evt })
    }
  }
  return merged
}

function formatToolCall(evt: { content: string; tool?: string; input?: Record<string, unknown> }): string {
  const toolName = evt.tool ?? evt.content.split(':')[0].trim()
  const args = evt.input ?? {}

  const arg =
    (typeof args.command === 'string' ? args.command.slice(0, 80) : null) ??
    (typeof args.file_path === 'string' ? args.file_path.split('/').pop() : null) ??
    (typeof args.pattern === 'string' ? args.pattern : null) ??
    (typeof args.query === 'string' ? args.query.slice(0, 60) : null) ??
    (typeof args.url === 'string' ? args.url.slice(0, 60) : null) ??
    (typeof args.prompt === 'string' ? args.prompt.slice(0, 40) : null) ??
    (typeof args.description === 'string' && !args.command ? args.description.slice(0, 60) : null) ??
    null

  return arg ? `${toolName} · ${arg}` : toolName
}

interface ToolGroup {
  event_type: 'tool_group'
  tools: WorkerEvent[]
  created_at: string
}

type DisplayEvent = WorkerEvent | ToolGroup

function groupConsecutiveTools(events: WorkerEvent[]): DisplayEvent[] {
  const result: DisplayEvent[] = []
  for (const evt of events) {
    if (evt.event_type === 'tool_use') {
      const last = result[result.length - 1]
      if (last && last.event_type === 'tool_group') {
        last.tools.push(evt)
      } else {
        result.push({ event_type: 'tool_group', tools: [evt], created_at: evt.created_at })
      }
    } else {
      result.push(evt)
    }
  }
  return result
}

// ── Linkify helper ───────────────────────────────────────────────────────

function linkify(text: string): (string | JSX.Element)[] {
  const parts = text.split(/(https?:\/\/[^\s]+)/g)
  return parts.map((part, i) =>
    /^https?:\/\//.test(part)
      ? <a key={i} href={part} target="_blank" rel="noopener noreferrer" className={styles.eventLink}>{part}</a>
      : part
  )
}

// ── Event row ────────────────────────────────────────────────────────────

interface EventRowProps {
  event_type: string
  content: string
  created_at: string
  tool?: string
  input?: Record<string, unknown>
}

function EventRow({ event_type, content, created_at, tool, input }: EventRowProps) {
  const time = formatTime(created_at)

  if (event_type === 'assistant_text') {
    const trimmed = content.trim()
    if (!trimmed) return null
    return (
      <div className={styles.eventRow}>
        <span className={styles.eventTime}>{time}</span>
        <div className={`${styles.eventBody} ${styles.eventAssistant}`}>
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{trimmed}</ReactMarkdown>
        </div>
      </div>
    )
  }

  if (event_type === 'user_message') {
    return (
      <div className={styles.eventRow}>
        <span className={styles.eventTime}>{time}</span>
        <div className={styles.eventBody}>
          <div className={styles.eventUserLabel}>You</div>
          <div className={styles.eventUserContent}>{linkify(content)}</div>
        </div>
      </div>
    )
  }

  if (event_type === 'tool_use') {
    return (
      <div className={`${styles.eventRow} ${styles.eventRowTool}`}>
        <span className={styles.eventTime}>{time}</span>
        <span className={styles.eventToolSep}>·</span>
        <span className={styles.eventTool}>{formatToolCall({ content, tool, input })}</span>
      </div>
    )
  }

  // fallback for unknown event types
  return (
    <div className={styles.eventRow}>
      <span className={styles.eventTime}>{time}</span>
      <div className={`${styles.eventBody} ${styles.eventAssistant}`}>
        <ReactMarkdown remarkPlugins={[remarkGfm]}>{content}</ReactMarkdown>
      </div>
    </div>
  )
}

// ── Tool group row ────────────────────────────────────────────────────────

interface ToolGroupRowProps {
  group: ToolGroup
  expanded: boolean
  onToggle: () => void
}

function ToolGroupRow({ group, expanded, onToggle }: ToolGroupRowProps) {
  const time = formatTime(group.created_at)
  const { tools } = group
  const MAX_PREVIEW = 3
  const preview = tools.slice(0, MAX_PREVIEW).map(t => formatToolCall(t))
  const extra = tools.length - MAX_PREVIEW

  return (
    <div className={styles.toolGroup} onClick={onToggle} data-testid="tool-group">
      <span className={styles.eventTime}>{time}</span>
      <div>
        <div className={styles.toolGroupSummary}>
          <span className={styles.toolGroupExpander}>{expanded ? '▼' : '▶'}</span>
          {expanded ? (
            <span>{tools.length} tool call{tools.length !== 1 ? 's' : ''}</span>
          ) : (
            <>
              <span>{preview.join(' · ')}</span>
              {extra > 0 && <span>+{extra} more</span>}
            </>
          )}
        </div>
        {expanded && (
          <div className={styles.toolGroupList}>
            {tools.map((t, i) => (
              <div key={i} className={styles.toolGroupItem}>
                · {formatToolCall(t)}
              </div>
            ))}
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
  return (
    <div className={styles.reviewCard} data-testid="review-card">
      <div className={styles.reviewCardHeader}>
        <span className={styles.reviewerName}>{review.reviewer}</span>
        <span className={`${styles.verdictBadge} ${verdictClass(review.verdict)}`}>
          {verdictLabel(review.verdict)}
        </span>
        <span className={styles.reviewTime}>{formatRelative(review.created_at)}</span>
      </div>
      <p className={styles.reviewSummary}>{review.summary}</p>
      {review.issues.length > 0 && (
        <ul className={styles.issueList}>
          {review.issues.map((issue, i) => (
            <li key={i} className={styles.issueItem}>
              <span className={`${styles.severityBadge} ${severityClass(issue.severity)}`}>
                {issue.severity}
              </span>
              <div className={styles.issueDetail}>
                <span className={styles.issueFile}>{issue.file}</span>
                <span className={styles.issueDesc}>{issue.description}</span>
              </div>
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

// ── Brief tab ────────────────────────────────────────────────────────────

function BriefSection({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className={styles.briefSection}>
      <div className={styles.briefLabel}>{label}</div>
      <div className={styles.briefRule} />
      <div className={styles.briefContent}>{children}</div>
    </div>
  )
}

function BriefTab({ brief, goal }: { brief: WorkerBrief | null; goal: string | null }) {
  if (!brief) {
    return (
      <div className={styles.briefEmpty}>
        No brief recorded for this worker.
      </div>
    )
  }

  return (
    <div className={styles.briefBody}>
      <BriefSection label="Goal">
        <p>{brief.goal ?? goal}</p>
      </BriefSection>

      {brief.context?.recent_changes && (
        <BriefSection label="Context">
          <p>{brief.context.recent_changes}</p>
        </BriefSection>
      )}

      {brief.constraints && brief.constraints.length > 0 && (
        <BriefSection label="Constraints">
          <ul className={styles.briefList}>
            {brief.constraints.map((c, i) => (
              <li key={i}>{c}</li>
            ))}
          </ul>
        </BriefSection>
      )}

      {brief.acceptance_criteria && brief.acceptance_criteria.length > 0 && (
        <BriefSection label="Acceptance Criteria">
          <ul className={styles.briefList}>
            {brief.acceptance_criteria.map((c, i) => (
              <li key={i}>{c}</li>
            ))}
          </ul>
        </BriefSection>
      )}
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
  const [activeTab, setActiveTab] = useState<Tab>('timeline')
  const [expandedGroups, setExpandedGroups] = useState<Set<number>>(new Set())
  const eventsEndRef = useRef<HTMLDivElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const pollRef = useRef<number | null>(null)
  const reviewingTimeoutRef = useRef<number | null>(null)
  const prevReviewCountRef = useRef<number>(0)

  const loadReviews = useCallback(async () => {
    try {
      const prevCount = prevReviewCountRef.current
      const r = await listWorkerReviews(workspace, workerId)
      setReviews(r)
      prevReviewCountRef.current = r.length
      // If a new review arrived while reviewing, clear reviewing state
      if (reviewing && r.length > prevCount) {
        setReviewing(false)
        if (reviewingTimeoutRef.current !== null) {
          window.clearTimeout(reviewingTimeoutRef.current)
          reviewingTimeoutRef.current = null
        }
        setActiveTab('reviews')
      }
    } catch {
      // silently ignore
    }
  }, [workspace, workerId, reviewing])

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
      loadReviews()
    }, 3000)

    return () => {
      if (pollRef.current !== null) {
        window.clearInterval(pollRef.current)
        pollRef.current = null
      }
      if (reviewingTimeoutRef.current !== null) {
        window.clearTimeout(reviewingTimeoutRef.current)
        reviewingTimeoutRef.current = null
      }
    }
  }, [load, loadReviews])

  // Auto-scroll to bottom when events arrive (only on timeline tab)
  useEffect(() => {
    if (activeTab === 'timeline' && data?.events?.length) {
      eventsEndRef.current?.scrollIntoView({ behavior: 'smooth' })
    }
  }, [data?.events?.length, activeTab])

  const handleSend = async () => {
    const textarea = textareaRef.current
    if (!textarea) return
    const message = textarea.value.trim()
    if (!message || sending) return

    setSending(true)
    try {
      await sendWorkerMessageV2(workspace, workerId, message)
      textarea.value = ''
      textarea.style.height = 'auto'
      await load(false)
    } catch (e) {
      console.error('send failed', e)
    } finally {
      setSending(false)
    }
  }

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
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
    // Safety timeout: clear reviewing after 90s
    reviewingTimeoutRef.current = window.setTimeout(() => {
      setReviewing(false)
      reviewingTimeoutRef.current = null
    }, 90000)
    try {
      await requestWorkerReview(workspace, workerId)
      // Poll reviews after short delay to catch fast completions
      window.setTimeout(() => {
        loadReviews()
      }, 2000)
    } catch (e) {
      console.error('review request failed', e)
      setReviewing(false)
      if (reviewingTimeoutRef.current !== null) {
        window.clearTimeout(reviewingTimeoutRef.current)
        reviewingTimeoutRef.current = null
      }
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
  const isTerminal = data.state === 'merged' || data.state === 'abandoned'
  const inputDisabled = data.state !== 'waiting' && data.state !== 'queued'

  return (
    <div className={styles.container}>
      {/* ── Header (fixed) ── */}
      <div className={styles.header}>
        <div className={styles.headerRow}>
          <h1 className={styles.goal}>
            {data.goal ?? data.branch ?? data.id}
          </h1>
          <div className={styles.headerActions}>
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
            {canReview && (
              <button
                className={styles.reviewBtn}
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
                className={styles.ghostBtnDanger}
                onClick={handleCancel}
                type="button"
              >
                Cancel
              </button>
            )}
            {canRequeue && (
              <button
                className={styles.ghostBtn}
                onClick={handleRequeue}
                type="button"
              >
                Retry
              </button>
            )}
          </div>
        </div>

        {/* Second row: status + branch + pills inline */}
        <div className={styles.headerMeta}>
          <StatusBadge worker={data} />
          {data.branch && (
            <span className={styles.branch}>{data.branch}</span>
          )}
          <Pills worker={data} />
          {data.revision_count > 0 && (
            <span className={styles.revisionPill}>
              Pass {data.revision_count}
            </span>
          )}
        </div>

        {/* Third row: PR link */}
        {data.pr_url && (
          <div className={styles.headerPrRow}>
            <a
              href={data.pr_url}
              target="_blank"
              rel="noopener noreferrer"
              className={styles.prLink}
            >
              {(() => {
                const prNumber = data.pr_url?.match(/\/pull\/(\d+)/)?.[1]
                return prNumber ? `#${prNumber}` : 'PR'
              })()} <ExternalLink size={12} />
            </a>
          </div>
        )}

        {reviewing && (
          <div className={styles.reviewingBanner}>
            Review in progress…
          </div>
        )}
      </div>

      {/* ── Tab bar ── */}
      <div className={styles.tabBar} role="tablist">
        <button
          className={`${styles.tab} ${activeTab === 'timeline' ? styles.tabActive : ''}`}
          onClick={() => setActiveTab('timeline')}
          type="button"
          role="tab"
          aria-selected={activeTab === 'timeline'}
          data-testid="tab-timeline"
        >
          Timeline
        </button>
        <button
          className={`${styles.tab} ${activeTab === 'reviews' ? styles.tabActive : ''}`}
          onClick={() => setActiveTab('reviews')}
          type="button"
          role="tab"
          aria-selected={activeTab === 'reviews'}
          data-testid="tab-reviews"
        >
          Reviews
          {reviews.length > 0 && (
            <span className={styles.tabBadge}>{reviews.length}</span>
          )}
        </button>
        <button
          className={`${styles.tab} ${activeTab === 'brief' ? styles.tabActive : ''}`}
          onClick={() => setActiveTab('brief')}
          type="button"
          role="tab"
          aria-selected={activeTab === 'brief'}
          data-testid="tab-brief"
        >
          Brief
        </button>
      </div>

      {/* ── Tab content (scrollable) ── */}
      <div key={activeTab} className={styles.tabContent}>
        {activeTab === 'timeline' && (
          <div className={styles.timelinePanel}>
            {/* Top divider: Worker started */}
            <div className={styles.stateDivider}>
              <span className={styles.stateDividerText}>
                Worker started · {formatTime(data.created_at)}
              </span>
            </div>

            {data.events && data.events.length > 0 ? (
              <div className={styles.events} data-testid="events-thread">
                {groupConsecutiveTools(mergeConsecutiveText(data.events)).map((ev, i) => {
                  if (ev.event_type === 'tool_group') {
                    const group = ev as ToolGroup
                    return (
                      <ToolGroupRow
                        key={i}
                        group={group}
                        expanded={expandedGroups.has(i)}
                        onToggle={() => {
                          setExpandedGroups(prev => {
                            const next = new Set(prev)
                            if (next.has(i)) {
                              next.delete(i)
                            } else {
                              next.add(i)
                            }
                            return next
                          })
                        }}
                      />
                    )
                  }
                  const e = ev as WorkerEvent
                  return (
                    <EventRow
                      key={i}
                      event_type={e.event_type}
                      content={e.content}
                      created_at={e.created_at}
                      tool={e.tool}
                      input={e.input}
                    />
                  )
                })}
                <div ref={eventsEndRef} />
              </div>
            ) : (
              <div className={styles.eventsEmpty}>No activity yet</div>
            )}

            {/* Bottom divider: current state */}
            {(['waiting', 'running', 'failed'].includes(data.state) || isTerminal) && (
              <div className={styles.stateDivider}>
                <span className={styles.stateDividerText}>
                  {stateDividerLabel(data.state, reviews.length > 0)}
                </span>
              </div>
            )}
          </div>
        )}

        {activeTab === 'reviews' && (
          <div className={styles.reviewsPanel} data-testid="reviews-section">
            {reviewing && (
              <div className={styles.reviewInProgress}>
                <div className={styles.reviewInProgressHeader}>
                  <span className={styles.reviewInProgressDot} />
                  <span className={styles.reviewInProgressLabel}>General · reviewing now</span>
                  <span className={styles.reviewInProgressTime}>started just now</span>
                </div>
                <div className={styles.reviewSkeletonLine} style={{ width: '80%' }} />
                <div className={styles.reviewSkeletonLine} style={{ width: '55%' }} />
              </div>
            )}
            {reviews.length === 0 ? (
              <div className={styles.reviewsEmpty}>
                <p className={styles.reviewsEmptyText}>No reviews yet.</p>
                {canReview && (
                  <button
                    className={styles.reviewBtn}
                    onClick={handleRequestReview}
                    disabled={reviewing}
                    type="button"
                  >
                    {reviewing ? 'Reviewing…' : 'Request Review'}
                  </button>
                )}
              </div>
            ) : (
              <div className={styles.reviewsList}>
                {[...reviews].reverse().map((review) => (
                  <ReviewCard key={review.id} review={review} />
                ))}
              </div>
            )}
          </div>
        )}

        {activeTab === 'brief' && (
          <div className={styles.briefPanel}>
            <BriefTab brief={data.brief} goal={data.goal} />
          </div>
        )}
      </div>

      {/* ── Input bar (Timeline tab only, hidden when terminal) ── */}
      {activeTab === 'timeline' && !isTerminal && (
        <div className={styles.actionBar} data-testid="action-bar">
          <div className={styles.inputRow}>
            <textarea
              ref={textareaRef}
              className={styles.textarea}
              placeholder={inputDisabled ? 'Worker is running…' : 'Send a message to the worker...'}
              rows={1}
              enterKeyHint="enter"
              disabled={inputDisabled}
              onKeyDown={handleKeyDown}
              onChange={(e) => {
                e.target.style.height = 'auto'
                e.target.style.height = `${Math.min(e.target.scrollHeight, 120)}px`
              }}
              style={inputDisabled ? { opacity: 0.4 } : undefined}
            />
            <button
              className={styles.sendBtn}
              onMouseDown={(e) => e.preventDefault()}
              onClick={handleSend}
              disabled={sending || inputDisabled}
              type="button"
            >
              Send
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
