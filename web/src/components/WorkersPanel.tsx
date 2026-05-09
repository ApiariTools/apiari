import type { Worker } from "@apiari/types";
import { ToolPanel } from "@apiari/ui";
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

function labelForAttemptRole(role?: string | null): string | null {
  if (!role) return null;
  if (role === "implementation") return "Implementation";
  if (role === "reviewer") return "Reviewer";
  if (role === "investigator") return "Investigator";
  return role;
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
        <div key={w.id} className={styles.card} onClick={() => onSelectWorker(w.id)}>
          {(() => {
            const lifecycleState = w.task_lifecycle_state ?? w.task_stage;
            const latestAttempt = w.latest_attempt ?? null;
            const latestAttemptLabel = latestAttempt
              ? `${labelForAttemptRole(latestAttempt.role) ?? "Attempt"} ${latestAttempt.state}`
              : null;
            return (
              <>
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
                  <span className={styles.time}>{formatElapsed(w.elapsed_secs)}</span>
                </div>
                <div className={styles.desc}>
                  {w.task_title || w.description || branchName(w.branch)}
                </div>
                <div className={styles.tags}>
                  {w.status === "stalled" && (
                    <span className={`${styles.tag} ${styles.tagWarn}`}>Stalled</span>
                  )}
                  {lifecycleState && <span className={styles.tag}>{lifecycleState}</span>}
                  {latestAttemptLabel && (
                    <span className={`${styles.tag} ${styles.tagAttempt}`}>
                      {latestAttemptLabel}
                    </span>
                  )}
                  {w.has_uncommitted_changes && (
                    <span className={`${styles.tag} ${styles.tagWarn}`}>Uncommitted diff</span>
                  )}
                  {!w.ready_branch && w.has_uncommitted_changes && (
                    <span className={`${styles.tag} ${styles.tagWarnSoft}`}>No ready branch</span>
                  )}
                  {w.task_repo && (
                    <span className={`${styles.tag} ${styles.tagBot}`}>repo: {w.task_repo}</span>
                  )}
                  {w.pr_url && (
                    <a
                      href={w.pr_url}
                      className={`${styles.tag} ${styles.tagPr}`}
                      onClick={(e) => e.stopPropagation()}
                      target="_blank"
                      rel="noopener noreferrer"
                    >
                      {w.pr_title ? `PR: ${w.pr_title}` : `PR`}
                    </a>
                  )}
                  {w.dispatched_by && (
                    <span className={`${styles.tag} ${styles.tagBot}`}>via {w.dispatched_by}</span>
                  )}
                </div>
                {latestAttempt?.detail ? (
                  <div className={styles.attemptDetail}>{latestAttempt.detail}</div>
                ) : null}
              </>
            );
          })()}
        </div>
      ))}
    </ToolPanel>
  );
}
