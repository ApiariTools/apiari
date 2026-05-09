import { useEffect, useRef, useState } from "react";
import { ChevronDown, Plus, LayoutDashboard } from "lucide-react";
import styles from "./Sidebar.module.css";

export interface SidebarTag {
  label: string;
  color: "green" | "amber" | "faint";
}

export interface SidebarItem {
  id: string;
  name: string;
  status: string; // 'running' | 'waiting' | 'stalled' | 'failed' | 'merged' | 'idle'
  meta?: string;
  tags?: SidebarTag[];
}

export interface ActivityItem {
  label: string;
  color: string;
}

export interface SidebarProps {
  selectedType: "auto_bot" | "worker" | null;
  selectedId: string | null;
  onSelect: (type: "auto_bot" | "worker", id: string) => void;
  onHome: () => void;
  autoBots: SidebarItem[];
  workers: SidebarItem[];
  workspaces: string[];
  workspace: string;
  onWorkspaceChange: (ws: string) => void;
  onQuickDispatch?: () => void;
  /** Number of workers hidden because they are in a terminal state (merged/abandoned/failed). */
  doneWorkerCount?: number;
  onShowDoneWorkers?: () => void;
  doneWorkersSelected?: boolean;
  /** Running background activity items shown at the bottom of the sidebar. */
  activityItems?: ActivityItem[];
}

function dotClass(status: string): string {
  switch (status) {
    case "running":
      return styles.dotRunning;
    case "waiting":
      return styles.dotWaiting;
    case "stalled":
      return styles.dotStalled;
    case "done":
      return styles.dotDone;
    default:
      return styles.dotIdle;
  }
}

interface ItemProps {
  item: SidebarItem;
  type: "auto_bot" | "worker";
  isSelected: boolean;
  onSelect: (type: "auto_bot" | "worker", id: string) => void;
}

function tagColorClass(color: SidebarTag["color"]): string {
  switch (color) {
    case "green":
      return styles.tagGreen;
    case "amber":
      return styles.tagAmber;
    default:
      return styles.tagFaint;
  }
}

function SidebarItemRow({ item, type, isSelected, onSelect }: ItemProps) {
  const showSecondLine = item.meta || (item.tags && item.tags.length > 0);
  return (
    <button
      className={`${styles.item} ${isSelected ? styles.itemSelected : ""} ${showSecondLine ? styles.itemTall : ""}`}
      onClick={() => onSelect(type, item.id)}
      type="button"
      aria-current={isSelected ? "true" : undefined}
    >
      <span className={`${styles.dot} ${dotClass(item.status)}`} aria-hidden="true" />
      <div className={styles.itemContent}>
        <span className={`${styles.name} ${isSelected ? styles.nameSelected : ""}`}>
          {item.name}
        </span>
        {showSecondLine && (
          <div className={styles.itemMeta}>
            {item.meta && <span className={styles.metaId}>{item.meta}</span>}
            {item.tags?.map((tag, i) => (
              <span key={i} className={`${styles.tag} ${tagColorClass(tag.color)}`}>
                {tag.label}
              </span>
            ))}
          </div>
        )}
      </div>
    </button>
  );
}

interface WorkspaceSelectorProps {
  workspaces: string[];
  workspace: string;
  onWorkspaceChange: (ws: string) => void;
}

function WorkspaceSelector({ workspaces, workspace, onWorkspaceChange }: WorkspaceSelectorProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  return (
    <div className={styles.workspaceSelector} ref={ref}>
      <button
        className={styles.workspaceTrigger}
        onClick={() => setOpen((o) => !o)}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <span className={styles.workspaceOrb} aria-hidden="true" />
        <span className={styles.workspaceName}>{workspace || "Loading..."}</span>
        <ChevronDown size={12} className={styles.workspaceChevron} />
      </button>
      {open && (
        <div className={styles.workspaceDropdown} role="listbox" aria-label="Select workspace">
          {workspaces.map((ws) => (
            <button
              key={ws}
              className={`${styles.workspaceOption} ${ws === workspace ? styles.workspaceOptionActive : ""}`}
              onClick={() => {
                onWorkspaceChange(ws);
                setOpen(false);
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
  );
}

export default function Sidebar({
  selectedType,
  selectedId,
  onSelect,
  onHome,
  autoBots,
  workers,
  workspaces,
  workspace,
  onWorkspaceChange,
  onQuickDispatch,
  doneWorkerCount = 0,
  onShowDoneWorkers,
  doneWorkersSelected = false,
  activityItems,
}: SidebarProps) {
  const homeSelected = selectedType === null && selectedId === null;
  return (
    <nav className={styles.sidebar} aria-label="Sidebar">
      <WorkspaceSelector
        workspaces={workspaces}
        workspace={workspace}
        onWorkspaceChange={onWorkspaceChange}
      />
      <div className={styles.selectorDivider} />
      <button
        className={`${styles.item} ${homeSelected ? styles.itemSelected : ""}`}
        onClick={onHome}
        type="button"
      >
        <LayoutDashboard size={14} className={styles.itemIcon} aria-hidden="true" />
        <div className={styles.itemContent}>
          <span className={`${styles.name} ${homeSelected ? styles.nameSelected : ""}`}>
            Overview
          </span>
        </div>
      </button>
      <div className={styles.divider} />
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
              isSelected={selectedType === "auto_bot" && selectedId === bot.id}
              onSelect={onSelect}
            />
          ))
        )}
      </div>
      <div className={styles.divider} />
      <div className={styles.section}>
        <div className={styles.sectionHeader}>
          <span className={styles.sectionLabel}>Workers</span>
          {onQuickDispatch && (
            <button
              type="button"
              className={styles.addBtn}
              onClick={onQuickDispatch}
              aria-label="New worker"
              data-testid="quick-dispatch-trigger"
            >
              <Plus size={14} />
            </button>
          )}
        </div>
        {workers.length === 0 && doneWorkerCount === 0 ? (
          <p className={styles.emptyMessage}>No workers yet</p>
        ) : (
          workers.map((worker) => (
            <SidebarItemRow
              key={worker.id}
              item={worker}
              type="worker"
              isSelected={selectedType === "worker" && selectedId === worker.id}
              onSelect={onSelect}
            />
          ))
        )}
        {doneWorkerCount > 0 && (
          <button
            type="button"
            className={`${styles.doneFooter} ${doneWorkersSelected ? styles.doneFooterSelected : ""}`}
            data-testid="done-workers-footer"
            onClick={onShowDoneWorkers}
          >
            {doneWorkerCount} completed
          </button>
        )}
      </div>
      {activityItems && activityItems.length > 0 && (
        <div className={styles.activityStrip}>
          {activityItems.map((item, i) => (
            <div key={i} className={styles.activityItem}>
              <span className={styles.activityDot} style={{ background: item.color }} />
              <span className={styles.activityLabel}>{item.label}</span>
            </div>
          ))}
        </div>
      )}
    </nav>
  );
}
