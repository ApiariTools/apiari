import { useEffect, useState, useCallback } from 'react'
import { LayoutDashboard, MessageSquare } from 'lucide-react'
import type { WorkerV2, AutoBot, DashboardWidget, Repo, ContextBotContext } from '../../types'
import { listWidgets, getRepos } from '../../api'
import { getWorkerTitle } from '../../utils/workerTitle'
import { repoSyncLabel } from '../../repoSync'
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

// ── Repo summary ──────────────────────────────────────────────────────────

function RepoSummary({ repos }: { repos: Repo[] }) {
  if (repos.length === 0) return null
  return (
    <div className={styles.builtinSection}>
      <span className={styles.attentionHeading}>Repos</span>
      {repos.map((repo) => {
        const syncLabel = repoSyncLabel(repo)
        const outOfSync = (repo.behind_count ?? 0) > 0 || (repo.ahead_count ?? 0) > 0
        return (
          <div key={repo.path} className={styles.repoRow}>
            <span
              className={styles.attentionDot}
              style={{ background: repo.is_clean ? 'var(--status-merged)' : 'var(--accent)' }}
            />
            <span className={styles.attentionName}>{repo.name}</span>
            <span className={styles.repoBranch}>{repo.branch}</span>
            {!repo.is_clean && <span className={styles.repoTag}>modified</span>}
            {outOfSync && <span className={styles.repoTagWarn}>{syncLabel}</span>}
          </div>
        )
      })}
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
  onOpenContextBot?: (context: ContextBotContext, title: string) => void
}

export default function Dashboard({ workspace, workers, onSelectWorker, onOpenContextBot }: DashboardProps) {
  const [widgets, setWidgets] = useState<DashboardWidget[]>([])
  const [repos, setRepos] = useState<Repo[]>([])

  const fetchWidgets = useCallback(() => {
    if (!workspace) return
    listWidgets(workspace).then(setWidgets).catch((e) => console.error('[dashboard] fetch widgets:', e))
  }, [workspace])

  const fetchRepos = useCallback(() => {
    if (!workspace) return
    getRepos(workspace).then(setRepos).catch(() => {})
  }, [workspace])

  useEffect(() => {
    fetchWidgets()
    const interval = setInterval(fetchWidgets, 30_000)
    return () => clearInterval(interval)
  }, [fetchWidgets])

  useEffect(() => {
    fetchRepos()
    const interval = setInterval(fetchRepos, 30_000)
    return () => clearInterval(interval)
  }, [fetchRepos])

  // Debug
  useEffect(() => {
    console.log('[dashboard] widgets:', widgets.length, widgets.map(w => w.type))
  }, [widgets])

  const activeWorkers = workers.filter((w) => w.state !== 'done' && w.state !== 'abandoned')
  const stalledWorkers = workers.filter((w) => w.state === 'stalled')

  // Separate alert_banners (always shown first) from the rest
  const alerts  = widgets.filter((w) => w.type === 'alert_banner')
  const rest    = widgets.filter((w) => w.type !== 'alert_banner')

  return (
    <div className={styles.container}>
      {/* Header */}
      <div className={styles.header}>
        <LayoutDashboard size={16} className={styles.headerIcon} />
        <span className={styles.headerTitle}>Overview</span>
        <button
            className={styles.askBtn}
            type="button"
            onClick={() => onOpenContextBot?.(
              {
                view: 'dashboard',
                entity_id: null,
                entity_snapshot: {
                  workspace,
                  active_worker_count: activeWorkers.length,
                  stalled_worker_count: stalledWorkers.length,
                  total_worker_count: workers.length,
                  repo_count: repos.length,
                  repos: repos.map((r) => ({ name: r.name, branch: r.branch, is_clean: r.is_clean })),
                },
              },
              workspace,
            )}
            title="Ask about this workspace"
          >
            <MessageSquare size={14} />
            <span>Ask</span>
          </button>
      </div>

      {/* Alert banners — always at top */}
      {alerts.length > 0 && (
        <div className={styles.alerts}>
          {alerts.map((w) => <Widget key={w.slot} widget={w} />)}
        </div>
      )}

      {/* Built-in repo summary */}
      <RepoSummary repos={repos} />

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
