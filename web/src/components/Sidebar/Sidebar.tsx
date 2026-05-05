import { useEffect, useRef, useState } from 'react'
import { ChevronDown } from 'lucide-react'
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
  workspaces: string[]
  workspace: string
  onWorkspaceChange: (ws: string) => void
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

interface WorkspaceSelectorProps {
  workspaces: string[]
  workspace: string
  onWorkspaceChange: (ws: string) => void
}

function WorkspaceSelector({ workspaces, workspace, onWorkspaceChange }: WorkspaceSelectorProps) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', handleClick)
    return () => document.removeEventListener('mousedown', handleClick)
  }, [open])

  return (
    <div className={styles.workspaceSelector} ref={ref}>
      <button
        className={styles.workspaceTrigger}
        onClick={() => setOpen((o) => !o)}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <span className={styles.workspaceName}>{workspace || 'Loading...'}</span>
        <ChevronDown size={14} className={styles.workspaceChevron} />
      </button>
      {open && (
        <div className={styles.workspaceDropdown} role="listbox" aria-label="Select workspace">
          {workspaces.map((ws) => (
            <button
              key={ws}
              className={`${styles.workspaceOption} ${ws === workspace ? styles.workspaceOptionActive : ''}`}
              onClick={() => {
                onWorkspaceChange(ws)
                setOpen(false)
              }}
              type="button"
              role="option"
              aria-selected={ws === workspace}
            >
              {ws}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

export default function Sidebar({
  selectedType,
  selectedId,
  onSelect,
  autoBots,
  workers,
  workspaces,
  workspace,
  onWorkspaceChange,
}: SidebarProps) {
  return (
    <nav className={styles.sidebar} aria-label="Sidebar">
      <WorkspaceSelector
        workspaces={workspaces}
        workspace={workspace}
        onWorkspaceChange={onWorkspaceChange}
      />
      <div className={styles.selectorDivider} />
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
