import { Bot, Cpu } from 'lucide-react';
import styles from './Sidebar.module.css';

interface SidebarProps {
  selectedId: string | null;
  onSelect: (type: 'auto_bot' | 'worker', id: string) => void;
}

// Hardcoded stubs for Phase 1B
const AUTO_BOTS = [
  { id: 'triage', name: 'Triage', status: 'running' as const, trigger: 'signal' },
  { id: 'standup', name: 'Standup', status: 'waiting' as const, trigger: 'cron' },
];

const WORKERS = [
  { id: 'fix-auth', name: 'fix-auth', branch: 'fix/auth-middleware', status: 'running' as const },
  { id: 'rate-limit', name: 'rate-limit', branch: 'feat/rate-limit', status: 'waiting' as const },
  { id: 'update-deps', name: 'update-deps', branch: 'chore/deps', status: 'merged' as const },
];

const STATUS_COLORS: Record<string, string> = {
  running: 'var(--status-running)',
  waiting: 'var(--status-waiting)',
  stalled: 'var(--status-stalled)',
  failed: 'var(--status-failed)',
  merged: 'var(--status-merged)',
};

export function Sidebar({ selectedId, onSelect }: SidebarProps) {
  return (
    <nav className={styles.root}>
      <div className={styles.header}>
        <span className={styles.logo}>hive</span>
      </div>

      <section className={styles.section}>
        <div className={styles.sectionLabel}>
          <Bot size={12} />
          Auto Bots
        </div>
        {AUTO_BOTS.map((bot) => (
          <button
            key={bot.id}
            className={`${styles.item} ${selectedId === bot.id ? styles.active : ''}`}
            onClick={() => onSelect('auto_bot', bot.id)}
          >
            <span className={styles.statusDot} style={{ background: STATUS_COLORS[bot.status] }} />
            <span className={styles.itemName}>{bot.name}</span>
            <span className={styles.itemMeta}>{bot.trigger}</span>
          </button>
        ))}
      </section>

      <div className={styles.divider} />

      <section className={styles.section}>
        <div className={styles.sectionLabel}>
          <Cpu size={12} />
          Workers
        </div>
        {WORKERS.map((worker) => (
          <button
            key={worker.id}
            className={`${styles.item} ${selectedId === worker.id ? styles.active : ''}`}
            onClick={() => onSelect('worker', worker.id)}
          >
            <span className={styles.statusDot} style={{ background: STATUS_COLORS[worker.status] }} />
            <span className={styles.itemName}>{worker.name}</span>
            <span className={styles.itemMeta}>{worker.branch}</span>
          </button>
        ))}
      </section>
    </nav>
  );
}
