import type { WorkspaceMode, WorkspaceModeDefinition } from "../consoleConfig";
import styles from "./MobileModeBar.module.css";

interface Props {
  modes: WorkspaceModeDefinition[];
  activeMode: WorkspaceMode;
  onSelectMode: (mode: WorkspaceMode) => void;
  badgeCounts?: Partial<Record<WorkspaceMode, number | null>>;
}

export function MobileModeBar({ modes, activeMode, onSelectMode, badgeCounts = {} }: Props) {
  return (
    <nav className={styles.bar} aria-label="Mobile workspace modes">
      {modes.map((mode) => {
        const Icon = mode.icon;
        const count = badgeCounts[mode.id] ?? null;
        return (
          <div key={mode.id} className={styles.buttonWrap}>
            <button
              className={`${styles.button} ${activeMode === mode.id ? styles.active : ""}`}
              onClick={() => onSelectMode(mode.id)}
              aria-label={`Open ${mode.label}`}
            >
              <Icon size={17} className={styles.icon} />
              <span className={styles.label}>{mode.label}</span>
            </button>
            {count ? <span className={styles.badge}>{count}</span> : null}
          </div>
        );
      })}
    </nav>
  );
}
