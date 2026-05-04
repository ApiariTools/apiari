import { ExternalLink } from "lucide-react";
import { ModeScaffold } from "../primitives/ModeScaffold";
import { PageHeader } from "../primitives/PageHeader";
import { EmptyState } from "../primitives/EmptyState";
import { StatusBadge } from "../primitives/StatusBadge";
import type { Task, Worker } from "../types";
import styles from "./TasksMode.module.css";

interface Props {
  tasks: Task[];
  workers: Worker[];
  onSelectWorker: (id: string) => void;
}

const ACTIVE_STAGES = ["Triage", "In Progress", "In AI Review", "Human Review"] as const;
const TERMINAL_STAGES = ["Merged", "Dismissed"] as const;

function formatTaskTime(value?: string | null) {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function toneForStage(stage: string): "accent" | "success" | "neutral" | "danger" {
  if (stage === "Human Review") return "accent";
  if (stage === "Merged") return "success";
  if (stage === "Dismissed") return "danger";
  return "neutral";
}

export function TasksMode({ tasks, workers, onSelectWorker }: Props) {
  const activeTasks = tasks.filter((task) => !TERMINAL_STAGES.includes(task.stage as (typeof TERMINAL_STAGES)[number]));
  const terminalTasks = tasks
    .filter((task) => TERMINAL_STAGES.includes(task.stage as (typeof TERMINAL_STAGES)[number]))
    .sort((a, b) => (b.resolved_at || b.updated_at).localeCompare(a.resolved_at || a.updated_at))
    .slice(0, 8);
  const humanReviewCount = tasks.filter((task) => task.stage === "Human Review").length;
  const openPrCount = tasks.filter((task) => Boolean(task.pr_url)).length;

  return (
    <ModeScaffold
      scrollBody
      hideHeaderOnMobile
      header={(
        <PageHeader
          eyebrow="Lifecycle"
          title="Review"
          summary="Track task progression, review handoffs, and PR state without mixing it into worker execution details."
          meta={(
            <div className={styles.meta}>
              <StatusBadge tone={humanReviewCount > 0 ? "accent" : "neutral"}>
                {humanReviewCount} in human review
              </StatusBadge>
              <StatusBadge tone="neutral">{activeTasks.length} active tasks</StatusBadge>
              <StatusBadge tone="success">{openPrCount} tasks with PRs</StatusBadge>
            </div>
          )}
        />
      )}
    >
      <div className={styles.page}>
        {tasks.length === 0 ? (
          <EmptyState
            title="No tasks yet"
            body="Tasks show lifecycle state like triage, review, and merge. Worker execution stays in Workers."
          />
        ) : (
          <>
            <section className={styles.board}>
              {ACTIVE_STAGES.map((stage) => {
                const stageTasks = activeTasks.filter((task) => task.stage === stage);
                return (
                  <div key={stage} className={styles.column}>
                    <div className={styles.columnHeader}>
                      <h2>{stage}</h2>
                      <span className={styles.columnCount}>{stageTasks.length}</span>
                    </div>
                    <div className={styles.columnBody}>
                      {stageTasks.length === 0 ? (
                        <div className={styles.emptyColumn}>Nothing here.</div>
                      ) : (
                        stageTasks.map((task) => {
                          const linkedWorker = task.worker_id
                            ? workers.find((worker) => worker.id === task.worker_id)
                            : null;
                          return (
                            <article key={task.id} className={styles.card}>
                              <div className={styles.cardTop}>
                                <StatusBadge tone={toneForStage(task.stage)}>{task.stage}</StatusBadge>
                                {task.repo ? <span className={styles.repo}>{task.repo}</span> : null}
                              </div>
                              <h3 className={styles.title}>{task.title}</h3>
                              <div className={styles.metaRow}>
                                {task.source ? <span>source: {task.source}</span> : null}
                                <span>updated: {formatTaskTime(task.updated_at)}</span>
                              </div>
                              <div className={styles.actions}>
                                {task.worker_id ? (
                                  <button
                                    className={styles.inlineButton}
                                    onClick={() => onSelectWorker(task.worker_id!)}
                                  >
                                    {linkedWorker ? "Open worker" : `Open worker ${task.worker_id}`}
                                  </button>
                                ) : null}
                                {task.pr_url ? (
                                  <a
                                    className={styles.link}
                                    href={task.pr_url}
                                    target="_blank"
                                    rel="noreferrer"
                                  >
                                    Open PR
                                    <ExternalLink size={13} />
                                  </a>
                                ) : null}
                              </div>
                            </article>
                          );
                        })
                      )}
                    </div>
                  </div>
                );
              })}
            </section>

            {terminalTasks.length > 0 ? (
              <section className={styles.historySection}>
                <div className={styles.historyHeader}>
                  <h2>Recent closed</h2>
                  <span>{terminalTasks.length} shown</span>
                </div>
                <div className={styles.historyList}>
                  {terminalTasks.map((task) => (
                    <article key={task.id} className={styles.historyCard}>
                      <div className={styles.historyTop}>
                        <strong>{task.title}</strong>
                        <StatusBadge tone={toneForStage(task.stage)}>{task.stage}</StatusBadge>
                      </div>
                      <div className={styles.metaRow}>
                        {task.repo ? <span>repo: {task.repo}</span> : null}
                        <span>{formatTaskTime(task.resolved_at || task.updated_at)}</span>
                      </div>
                    </article>
                  ))}
                </div>
              </section>
            ) : null}
          </>
        )}
      </div>
    </ModeScaffold>
  );
}
