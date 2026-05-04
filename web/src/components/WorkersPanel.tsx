import type { Worker } from "../types";
import { ToolPanel } from "../primitives/ToolPanel";
import styles from "./WorkersPanel.module.css";

interface Props {
  workers: Worker[];
  onSelectWorker: (id: string) => void;
  mobileOpen?: boolean;
  onClose?: () => void;
}

function formatElapsed(secs: number | null): string {
  if (secs == null) return "";
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} min`;
  const hrs = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem > 0 ? `${hrs}h ${rem}m` : `${hrs}h`;
}

function branchName(branch: string): string {
  return branch.replace(/^swarm\//, "");
}

export function WorkersPanel({ workers, onSelectWorker, mobileOpen, onClose }: Props) {
  return (
    <ToolPanel
      title="Workers"
      subtitle="Execution status, active branches, and handoff context for autonomous work."
      mobileOpen={mobileOpen}
      onClose={onClose}
    >
      {workers.map((w) => (
        <div
          key={w.id}
          className={styles.card}
          onClick={() => onSelectWorker(w.id)}
        >
          <div className={styles.top}>
            <span
              className={`${styles.dot} ${w.status === "running" || w.status === "active" ? styles.running : ""}`}
              style={{
                background:
                  w.status === "running" || w.status === "active"
                    ? "var(--green)"
                    : w.status === "waiting"
                      ? "var(--accent)"
                      : "var(--text-faint)",
              }}
            />
            <span className={styles.id}>{w.id}</span>
            <span className={styles.time}>
              {formatElapsed(w.elapsed_secs)}
            </span>
          </div>
          <div className={styles.desc}>
            {w.description || branchName(w.branch)}
          </div>
          <div className={styles.tags}>
            {w.pr_url && (
              <a
                href={w.pr_url}
                className={`${styles.tag} ${styles.tagPr}`}
                onClick={(e) => e.stopPropagation()}
                target="_blank"
                rel="noopener noreferrer"
              >
                {w.pr_title
                  ? `PR: ${w.pr_title}`
                  : `PR`}
              </a>
            )}
            {w.dispatched_by && (
              <span className={`${styles.tag} ${styles.tagBot}`}>
                via {w.dispatched_by}
              </span>
            )}
          </div>
        </div>
      ))}
    </ToolPanel>
  );
}
