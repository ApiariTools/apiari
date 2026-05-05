import type { WorkerV2, AutoBot } from '../../types'
import { formatRelative } from '../../utils/time'
import EmptyState from '../EmptyState/EmptyState'
import styles from './Dashboard.module.css'

// ── Helpers ──────────────────────────────────────────────────────────────

function getHour(): number {
  return new Date().getHours()
}

function greeting(): string {
  const h = getHour()
  if (h < 12) return 'Good morning.'
  if (h < 17) return 'Good afternoon.'
  return 'Good evening.'
}

function workerDotClass(worker: WorkerV2): string {
  if (worker.is_stalled) return styles.dotStalled
  switch (worker.state) {
    case 'running': return styles.dotRunning
    case 'waiting': return styles.dotWaiting
    case 'failed': return styles.dotFailed
    case 'merged': return styles.dotMerged
    default: return styles.dotIdle
  }
}

function autoBotDotClass(bot: AutoBot): string {
  switch (bot.status) {
    case 'running': return styles.dotRunning
    case 'error': return styles.dotFailed
    default: return styles.dotIdle
  }
}

function workerStateLabel(worker: WorkerV2): string {
  if (worker.is_stalled) return 'Stalled'
  return worker.label || worker.state
}

// ── Stat card ────────────────────────────────────────────────────────────

interface StatCardProps {
  label: string
  count: number
  colorClass: string
}

function StatCard({ label, count, colorClass }: StatCardProps) {
  return (
    <div className={styles.statCard}>
      <span className={`${styles.statCount} ${colorClass}`}>{count}</span>
      <span className={styles.statLabel}>{label}</span>
    </div>
  )
}

// ── Worker row ───────────────────────────────────────────────────────────

interface WorkerRowProps {
  worker: WorkerV2
  onClick: () => void
}

function WorkerRow({ worker, onClick }: WorkerRowProps) {
  const dotCls = workerDotClass(worker)
  const label = workerStateLabel(worker)
  const name = worker.goal ?? worker.branch ?? worker.id

  return (
    <button
      className={styles.itemRow}
      onClick={onClick}
      type="button"
      data-testid="dashboard-worker-row"
    >
      <span className={`${styles.dot} ${dotCls}`} aria-hidden="true" />
      <span className={styles.itemName}>{name}</span>
      <span className={styles.itemMeta}>{label}</span>
      {worker.revision_count > 0 && (
        <span className={styles.revPill}>pass {worker.revision_count}</span>
      )}
    </button>
  )
}

// ── Auto bot row ─────────────────────────────────────────────────────────

interface AutoBotRowProps {
  bot: AutoBot
  onClick: () => void
}

function AutoBotRow({ bot, onClick }: AutoBotRowProps) {
  const dotCls = autoBotDotClass(bot)

  let meta = bot.status
  if (bot.status === 'idle') {
    meta = 'idle'
  } else if (bot.status === 'running') {
    meta = 'running'
  }

  return (
    <button
      className={styles.itemRow}
      onClick={onClick}
      type="button"
      data-testid="dashboard-auto-bot-row"
    >
      <span className={`${styles.dot} ${dotCls}`} aria-hidden="true" />
      <span className={styles.itemName}>{bot.name}</span>
      <span className={styles.itemMeta}>{meta}</span>
      {bot.updated_at && bot.status !== 'running' && (
        <span className={styles.itemTimestamp}>
          {formatRelative(bot.updated_at)}
        </span>
      )}
    </button>
  )
}

// ── Main component ───────────────────────────────────────────────────────

export interface DashboardProps {
  workspace: string
  workers: WorkerV2[]
  autoBots: AutoBot[]
  onSelectWorker: (id: string) => void
  onSelectAutoBot: (id: string) => void
}

export default function Dashboard({
  workers,
  autoBots,
  onSelectWorker,
  onSelectAutoBot,
}: DashboardProps) {
  if (workers.length === 0 && autoBots.length === 0) {
    return <EmptyState />
  }

  const runningCount = workers.filter(
    (w) => w.state === 'running' && !w.is_stalled,
  ).length
  const waitingCount = workers.filter((w) => w.state === 'waiting').length
  const failedCount = workers.filter((w) => w.state === 'failed').length
  const mergedCount = workers.filter((w) => w.state === 'merged').length

  return (
    <div className={styles.container} data-testid="dashboard">
      <p className={styles.greeting}>{greeting()} Here's what's happening.</p>

      {/* Stat cards */}
      <div className={styles.stats} data-testid="stat-cards">
        <StatCard label="Running" count={runningCount} colorClass={styles.countRunning} />
        <StatCard label="Waiting" count={waitingCount} colorClass={styles.countWaiting} />
        <StatCard label="Failed" count={failedCount} colorClass={styles.countFailed} />
        <StatCard label="Merged" count={mergedCount} colorClass={styles.countMerged} />
      </div>

      {/* Workers */}
      {workers.length > 0 && (
        <section className={styles.section} data-testid="workers-section">
          <span className={styles.sectionLabel}>Workers</span>
          {workers.map((w) => (
            <WorkerRow
              key={w.id}
              worker={w}
              onClick={() => onSelectWorker(w.id)}
            />
          ))}
        </section>
      )}

      {/* Auto Bots */}
      {autoBots.length > 0 && (
        <section className={styles.section} data-testid="auto-bots-section">
          <span className={styles.sectionLabel}>Auto Bots</span>
          {autoBots.map((b) => (
            <AutoBotRow
              key={b.id}
              bot={b}
              onClick={() => onSelectAutoBot(b.id)}
            />
          ))}
        </section>
      )}
    </div>
  )
}
