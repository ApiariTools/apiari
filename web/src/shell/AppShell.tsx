import type { ReactNode } from "react";
import { TopBar } from "../components/TopBar";
import { WorkspaceNav } from "../components/WorkspaceNav";
import { MobileModeBar } from "../components/MobileModeBar";
import type { WorkspaceMode, WorkspaceModeDefinition } from "../consoleConfig";
import type { Workspace } from "../types";
import type { UsageData } from "../api";
import styles from "./AppShell.module.css";

interface Props {
  workspaces: Workspace[];
  activeWorkspace: string;
  activeRemote?: string;
  onSelectWorkspace: (name: string, remote?: string) => void;
  onMenuToggle: () => void;
  onOpenPalette: () => void;
  onToggleSimulator: () => void;
  usage: UsageData;
  isMobile: boolean;
  isTablet: boolean;
  menuOpen: boolean;
  onCloseMenu: () => void;
  visibleModes: WorkspaceModeDefinition[];
  activeMode: WorkspaceMode;
  onSelectMode: (mode: WorkspaceMode) => void;
  workerCount: number;
  repoCount: number;
  pendingFollowupCount: number;
  showMobileModeBar: boolean;
  mobileBadgeCounts: Partial<Record<WorkspaceMode, number | null>>;
  children: ReactNode;
}

export function AppShell({
  workspaces,
  activeWorkspace,
  activeRemote,
  onSelectWorkspace,
  onMenuToggle,
  onOpenPalette,
  onToggleSimulator,
  usage,
  isMobile,
  isTablet,
  menuOpen,
  onCloseMenu,
  visibleModes,
  activeMode,
  onSelectMode,
  workerCount,
  repoCount,
  pendingFollowupCount,
  showMobileModeBar,
  mobileBadgeCounts,
  children,
}: Props) {
  return (
    <>
      <TopBar
        workspaces={workspaces}
        active={activeWorkspace}
        activeRemote={activeRemote}
        isMobile={isMobile}
        onSelect={onSelectWorkspace}
        onMenuToggle={onMenuToggle}
        onOpenPalette={onOpenPalette}
        onToggleSimulator={onToggleSimulator}
        usage={usage}
      />
      <div className={styles.root}>
        {menuOpen && <div className="drawer-backdrop" onClick={onCloseMenu} />}
        {(!isTablet || menuOpen) && (
          <WorkspaceNav
            workspaces={workspaces}
            activeWorkspace={activeWorkspace}
            activeRemote={activeRemote}
            onSelectWorkspace={onSelectWorkspace}
            modes={visibleModes}
            activeMode={activeMode}
            onSelectMode={onSelectMode}
            workerCount={workerCount}
            repoCount={repoCount}
            pendingFollowupCount={pendingFollowupCount}
            mobileOpen={menuOpen}
          />
        )}
        <div className={`${styles.main} ${showMobileModeBar ? styles.mainMobilePadded : ""}`}>
          {children}
          {showMobileModeBar && (
            <MobileModeBar
              modes={visibleModes}
              activeMode={activeMode}
              onSelectMode={onSelectMode}
              badgeCounts={mobileBadgeCounts}
            />
          )}
        </div>
      </div>
    </>
  );
}
