import { CheckCircle2, XCircle, Loader2 } from "lucide-react";
import type { Repo, ResearchTask } from "../types";
import styles from "./ReposPanel.module.css";

interface Props {
  repos: Repo[];
  researchTasks?: ResearchTask[];
  onSelectWorker: (id: string) => void;
  mobileOpen?: boolean;
  onClose?: () => void;
}

function branchName(branch: string): string {
  return branch.replace(/^swarm\//, "");
}

export function ReposPanel({ repos, researchTasks, onSelectWorker, mobileOpen, onClose }: Props) {
  return (
    <>
      {mobileOpen && (
        <div className={styles.backdrop} onClick={onClose} />
      )}
      <div className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
        <div className={styles.title}>Repos</div>
        {repos.map((repo) => (
          <div key={repo.path} className={styles.repoRow}>
            <div className={styles.repoHeader}>
              <span
                className={styles.statusDot}
                style={{ background: repo.is_clean ? "var(--green)" : "var(--accent)" }}
              />
              <span className={styles.repoName}>{repo.name}</span>
              <span className={styles.repoBranch}>{repo.branch}</span>
              {!repo.is_clean && (
                <span className={styles.dirtyBadge}>modified</span>
              )}
            </div>
            {repo.workers.length > 0 && (
              <div className={styles.workerList}>
                {repo.workers.map((w) => (
                  <div
                    key={w.id}
                    className={styles.workerCard}
                    onClick={() => onSelectWorker(w.id)}
                  >
                    <div className={styles.workerTopLine}>
                      <span
                        className={styles.workerDot}
                        style={{
                          background:
                            w.status === "running" || w.status === "active"
                              ? "var(--green)"
                              : w.status === "waiting"
                                ? "var(--accent)"
                                : "var(--text-faint)",
                        }}
                      />
                      <span className={styles.workerId}>{w.id}</span>
                      <span className={styles.agentBadge} data-agent={w.agent.split(/[- ]/)[0].toLowerCase()}>
                        {w.agent}
                      </span>
                      {w.pr_url && <span className={styles.prBadge}>PR</span>}
                      {w.review_state && (
                        <span className={styles.reviewBadge} data-state={w.review_state.toLowerCase()}>
                          {w.review_state === "APPROVED" ? "Approved" :
                           w.review_state === "CHANGES_REQUESTED" ? "Changes" :
                           "Pending"}
                        </span>
                      )}
                      {w.open_comments != null && w.open_comments > 0 && (
                        <span className={styles.commentBadge}>
                          {w.open_comments} open{w.resolved_comments ? ` · ${w.resolved_comments} resolved` : ""}
                        </span>
                      )}
                      {w.ci_status && (
                        <span className={styles.ciBadge} data-status={w.ci_status.toLowerCase()}>
                          {w.ci_status === "SUCCESS" ? "CI ok" : w.ci_status === "FAILURE" ? "CI fail" : "CI ..."}
                        </span>
                      )}
                    </div>
                    <div className={styles.workerBranchLine}>{branchName(w.branch)}</div>
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
        {repos.length === 0 && (
          <div className={styles.empty}>No repos found</div>
        )}
        <div className={styles.title} style={{ marginTop: 16 }}>Research</div>
        {researchTasks && researchTasks.length > 0 ? (
          researchTasks.map((task) => (
            <div key={task.id} className={styles.repoRow}>
              <div className={styles.repoHeader}>
                {task.status === "running" ? (
                  <Loader2 size={14} className={styles.spinning} style={{ color: "var(--accent)", flexShrink: 0 }} />
                ) : task.status === "complete" ? (
                  <CheckCircle2 size={14} style={{ color: "var(--green)", flexShrink: 0 }} />
                ) : (
                  <XCircle size={14} style={{ color: "var(--red)", flexShrink: 0 }} />
                )}
                <span className={styles.repoName}>{task.topic}</span>
                {task.output_file && (
                  <span className={styles.repoBranch}>{task.output_file}</span>
                )}
              </div>
            </div>
          ))
        ) : (
          <div className={styles.emptyHint}>Use /research &lt;topic&gt; to start</div>
        )}
      </div>
    </>
  );
}
