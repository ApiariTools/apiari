import styles from './Sidebar.module.css'

export interface SidebarItem {
  id: string
  name: string
  status: string // 'running' | 'waiting' | 'stalled' | 'failed' | 'merged' | 'idle'
  meta?: string
}

export interface SidebarProps {
  selectedType: 'auto_bot' | 'worker' | null
  selectedId: string | null
  onSelect: (type: 'auto_bot' | 'worker', id: string) => void
  autoBots: SidebarItem[]
  workers: SidebarItem[]
}

function dotClass(status: string): string {
  switch (status) {
    case 'running': return styles.dotRunning
    case 'waiting': return styles.dotWaiting
    case 'stalled': return styles.dotStalled
    case 'failed': return styles.dotFailed
    case 'merged': return styles.dotMerged
    default: return styles.dotIdle
  }
}

interface ItemProps {
  item: SidebarItem
  type: 'auto_bot' | 'worker'
  isSelected: boolean
  onSelect: (type: 'auto_bot' | 'worker', id: string) => void
}

function SidebarItemRow({ item, type, isSelected, onSelect }: ItemProps) {
  return (
    <button
      className={`${styles.item} ${isSelected ? styles.itemSelected : ''}`}
      onClick={() => onSelect(type, item.id)}
      type="button"
      aria-current={isSelected ? 'true' : undefined}
    >
      <span className={`${styles.dot} ${dotClass(item.status)}`} aria-hidden="true" />
      <span className={`${styles.name} ${isSelected ? styles.nameSelected : ''}`}>
        {item.name}
      </span>
      {item.meta && (
        <span className={styles.meta}>{item.meta}</span>
      )}
    </button>
  )
}

export default function Sidebar({
  selectedType,
  selectedId,
  onSelect,
  autoBots,
  workers,
}: SidebarProps) {
  return (
    <nav className={styles.sidebar} aria-label="Sidebar">
      <div className={styles.section}>
        <span className={styles.sectionLabel}>Auto Bots</span>
        {autoBots.length === 0 ? (
          <p className={styles.emptyMessage}>No auto bots</p>
        ) : (
          autoBots.map((bot) => (
            <SidebarItemRow
              key={bot.id}
              item={bot}
              type="auto_bot"
              isSelected={selectedType === 'auto_bot' && selectedId === bot.id}
              onSelect={onSelect}
            />
          ))
        )}
      </div>
      <div className={styles.divider} />
      <div className={styles.section}>
        <span className={styles.sectionLabel}>Workers</span>
        {workers.length === 0 ? (
          <p className={styles.emptyMessage}>No workers yet</p>
        ) : (
          workers.map((worker) => (
            <SidebarItemRow
              key={worker.id}
              item={worker}
              type="worker"
              isSelected={selectedType === 'worker' && selectedId === worker.id}
              onSelect={onSelect}
            />
          ))
        )}
      </div>
    </nav>
  )
}
