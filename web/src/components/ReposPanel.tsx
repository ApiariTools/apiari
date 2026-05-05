import { useState } from "react";
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

type StatusFilter = "running" | "waiting" | "failed" | "merged" | null;

function branchName(branch: string): string {
  return branch.replace(/^swarm\//, "");
}

function matchesFilter(status: string, filter: StatusFilter): boolean {
  if (!filter) return true;
  if (filter === "running") return status === "running" || status === "active";
  if (filter === "waiting") return status === "waiting";
  if (filter === "failed") return status === "failed" || status === "error";
  if (filter === "merged") return status === "merged" || status === "done" || status === "complete";
  return true;
}

export function ReposPanel({ repos, researchTasks, onSelectWorker, mobileOpen, onClose }: Props) {
  const [filterStatus, setFilterStatus] = useState<StatusFilter>(null);

  const allWorkers = repos.flatMap((r) => r.workers);
  const counts = {
    running: allWorkers.filter((w) => w.status === "running" || w.status === "active").length,
    waiting: allWorkers.filter((w) => w.status === "waiting").length,
    failed: allWorkers.filter((w) => w.status === "failed" || w.status === "error").length,
    merged: allWorkers.filter((w) => w.status === "merged" || w.status === "done" || w.status === "complete").length,
  };

  const handleCardClick = (key: StatusFilter) => {
    setFilterStatus((prev) => (prev === key ? null : key));
  };

  return (
    <>
      {mobileOpen && (
        <div className={styles.backdrop} onClick={onClose} />
      )}
      <div className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
        <div className={styles.statCards}>
          {(
            [
              { key: "running" as StatusFilter, label: "Running", count: counts.running, color: "var(--green)" },
              { key: "waiting" as StatusFilter, label: "Waiting", count: counts.waiting, color: "var(--accent)" },
              { key: "failed" as StatusFilter, label: "Failed", count: counts.failed, color: "var(--red)" },
              { key: "merged" as StatusFilter, label: "Merged", count: counts.merged, color: "var(--text-faint)" },
            ] as const
          ).map(({ key, label, count, color }) => (
            <button
              key={key}
              className={`${styles.statCard} ${filterStatus === key ? styles.statCardActive : ""}`}
              onClick={() => handleCardClick(key)}
            >
              <span className={styles.statCount} style={{ color }}>{count}</span>
              <span className={styles.statLabel}>{label}</span>
            </button>
          ))}
        </div>
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
            {repo.workers.filter((w) => matchesFilter(w.status, filterStatus)).length > 0 && (
              <div className={styles.workerList}>
                {repo.workers.filter((w) => matchesFilter(w.status, filterStatus)).map((w) => (
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
