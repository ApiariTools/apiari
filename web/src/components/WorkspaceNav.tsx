import type { Workspace } from "../types";
import type { WorkspaceMode, WorkspaceModeDefinition } from "../consoleConfig";
import styles from "./WorkspaceNav.module.css";

interface ModeItem {
  mode: WorkspaceModeDefinition;
  count?: number | null;
}

interface Props {
  workspaces: Workspace[];
  activeWorkspace: string;
  activeRemote?: string;
  onSelectWorkspace: (name: string, remote?: string) => void;
  modes: WorkspaceModeDefinition[];
  activeMode: WorkspaceMode;
  onSelectMode: (mode: WorkspaceMode) => void;
  taskCount: number;
  workerCount: number;
  repoCount: number;
  pendingFollowupCount: number;
  mobileOpen?: boolean;
}

export function WorkspaceNav({
  workspaces,
  activeWorkspace,
  activeRemote,
  onSelectWorkspace,
  modes,
  activeMode,
  onSelectMode,
  taskCount,
  workerCount,
  repoCount,
  pendingFollowupCount,
  mobileOpen,
}: Props) {
  const items: ModeItem[] = modes.map((mode) => ({
    mode,
    count:
      mode.id === "overview" ? (pendingFollowupCount > 0 ? pendingFollowupCount : null)
        : mode.id === "tasks" ? (taskCount || null)
        : mode.id === "workers" ? (workerCount || null)
          : mode.id === "repos" ? (repoCount || null)
            : null,
  }));

  return (
    <aside className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
      <div className={styles.sectionLabel}>Workspace</div>
      <div className={styles.workspaceList}>
        {workspaces.map((workspace) => {
          const isActive = workspace.name === activeWorkspace && workspace.remote === activeRemote;
          return (
            <button
              key={`${workspace.remote || "local"}/${workspace.name}`}
              className={`${styles.workspaceBtn} ${isActive ? styles.activeWorkspace : ""}`}
              onClick={() => onSelectWorkspace(workspace.name, workspace.remote)}
              aria-label={workspace.remote ? `Open workspace ${workspace.name} (${workspace.remote})` : `Open workspace ${workspace.name}`}
            >
              <span className={styles.workspaceName}>{workspace.name}</span>
              {workspace.remote ? <span className={styles.workspaceRemote}>{workspace.remote}</span> : null}
            </button>
          );
        })}
      </div>
      <div className={styles.sectionDivider} />
      <div className={styles.sectionLabel}>Modes</div>
      <div className={styles.modeList}>
        {items.map(({ mode, count }) => {
          const Icon = mode.icon;
          return (
            <button
              key={mode.id}
              className={`${styles.modeBtn} ${activeMode === mode.id ? styles.active : ""}`}
              onClick={() => onSelectMode(mode.id)}
            >
              <span className={styles.modeLeft}>
                <Icon size={16} className={styles.modeIcon} />
                <span>{mode.label}</span>
              </span>
              {count ? <span className={styles.modeBadge}>{count}</span> : null}
            </button>
          );
        })}
      </div>

      <div className={styles.tip}>
        <span>Modes are global. Bot selection happens inside Chat.</span>
      </div>
    </aside>
  );
}
