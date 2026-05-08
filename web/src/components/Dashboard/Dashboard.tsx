import { useEffect, useState, useCallback } from 'react'
import { LayoutDashboard } from 'lucide-react'
import type { WorkerV2, AutoBot, DashboardWidget } from '../../types'
import { listWidgets } from '../../api'
import { getWorkerTitle } from '../../utils/workerTitle'
import Widget from '../widgets/Widget'
import styles from './Dashboard.module.css'

// ── Worker summary (built-in stat_row widget) ──────────────────────────────

function formatWorkerStatus(state: WorkerV2['state']): string {
  switch (state) {
    case 'done':
      return 'Completed'
    case 'abandoned':
      return 'Abandoned'
    case 'waiting':
      return 'Waiting'
    case 'stalled':
      return 'Stalled'
    case 'running':
      return 'Running'
    default:
      return state
  }
}

function sortNewestFirst(a: WorkerV2, b: WorkerV2): number {
  return b.updated_at.localeCompare(a.updated_at)
}

function WorkerSummary({ workers, onSelectWorker }: { workers: WorkerV2[]; onSelectWorker: (id: string) => void }) {
  const activeWorkers = workers.filter((w) => w.state !== 'done' && w.state !== 'abandoned')
  const recentWorkers = workers
    .filter((w) => w.state === 'done' || w.state === 'abandoned')
    .slice()
    .sort(sortNewestFirst)
  const running = activeWorkers.filter((w) => w.state === 'running')
  const waiting = activeWorkers.filter((w) => w.state === 'waiting')
  const stalled = activeWorkers.filter((w) => w.state === 'stalled')

  const attentionWorkers = [...stalled, ...waiting]

  return (
    <div className={styles.builtinSection}>
      {/* Stat pills */}
      <div className={styles.statPills}>
        {[
          { label: 'Running', count: running.length, color: 'var(--status-running)' },
          { label: 'Waiting', count: waiting.length, color: 'var(--status-waiting)' },
          { label: 'Stalled', count: stalled.length, color: 'var(--status-stalled)' },
        ].filter((s) => s.count > 0).map((s) => (
          <div key={s.label} className={styles.statPill}>
            <span className={styles.statPillNum} style={{ color: s.color }}>{s.count}</span>
            <span className={styles.statPillLabel}>{s.label}</span>
          </div>
        ))}
        {activeWorkers.length === 0 && <span className={styles.emptyMsg}>No active workers</span>}
      </div>

      {/* Attention list */}
      {attentionWorkers.length > 0 && (
        <div className={styles.attentionList}>
          <span className={styles.attentionHeading}>Needs attention</span>
          {attentionWorkers.map((w) => {
            const dotColor = w.state === 'stalled'
              ? 'var(--status-stalled)'
              : 'var(--status-waiting)'
            return (
              <button key={w.id} className={styles.attentionRow} onClick={() => onSelectWorker(w.id)}>
                <span className={styles.attentionDot} style={{ background: dotColor }} />
                <span className={styles.attentionName}>{getWorkerTitle(w)}</span>
                <span className={styles.attentionId}>{w.id}</span>
              </button>
            )
          })}
        </div>
      )}

      {recentWorkers.length > 0 && (
        <div className={styles.attentionList}>
          <span className={styles.attentionHeading}>Recent workers</span>
          {recentWorkers.slice(0, 8).map((w) => (
            <button key={w.id} className={styles.attentionRow} onClick={() => onSelectWorker(w.id)}>
              <span
                className={styles.attentionDot}
                style={{ background: w.state === 'abandoned' ? 'var(--status-abandoned)' : 'var(--status-merged)' }}
              />
              <span className={styles.attentionName}>{getWorkerTitle(w)}</span>
              <span
                className={`${styles.historyState} ${w.state === 'abandoned' ? styles.historyStateAbandoned : styles.historyStateDone}`}
              >
                {formatWorkerStatus(w.state)}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

// ── Main ──────────────────────────────────────────────────────────────────

export interface DashboardProps {
  workspace: string
  workers: WorkerV2[]
  autoBots: AutoBot[]
  onSelectWorker: (id: string) => void
  onSelectAutoBot: (id: string) => void
}

export default function Dashboard({ workspace, workers, onSelectWorker }: DashboardProps) {
  const [widgets, setWidgets] = useState<DashboardWidget[]>([])

  const fetchWidgets = useCallback(() => {
    if (!workspace) return
    listWidgets(workspace).then(setWidgets).catch((e) => console.error('[dashboard] fetch widgets:', e))
  }, [workspace])

  useEffect(() => {
    fetchWidgets()
    const interval = setInterval(fetchWidgets, 30_000)
    return () => clearInterval(interval)
  }, [fetchWidgets])

  // Debug
  useEffect(() => {
    console.log('[dashboard] widgets:', widgets.length, widgets.map(w => w.type))
  }, [widgets])

  // Separate alert_banners (always shown first) from the rest
  const alerts  = widgets.filter((w) => w.type === 'alert_banner')
  const rest    = widgets.filter((w) => w.type !== 'alert_banner')

  return (
    <div className={styles.container}>
      {/* Header */}
      <div className={styles.header}>
        <LayoutDashboard size={16} className={styles.headerIcon} />
        <span className={styles.headerTitle}>Overview</span>
      </div>

      {/* Alert banners — always at top */}
      {alerts.length > 0 && (
        <div className={styles.alerts}>
          {alerts.map((w) => <Widget key={w.slot} widget={w} />)}
        </div>
      )}

      {/* Built-in worker summary */}
      <WorkerSummary workers={workers} onSelectWorker={onSelectWorker} />

      {/* Bot-written widgets */}
      {rest.length > 0 && (
        <div className={styles.widgetGrid}>
          {rest.map((w) => <Widget key={w.slot} widget={w} />)}
        </div>
      )}

      {/* Empty widget state */}
      {widgets.length === 0 && (
        <div className={styles.widgetsEmpty}>
          <p className={styles.widgetsEmptyText}>No widgets yet.</p>
          <p className={styles.widgetsEmptyHint}>
            Bots can write widgets to this dashboard using the <code>write_dashboard_widget</code> tool.
          </p>
        </div>
      )}
    </div>
  )
}
