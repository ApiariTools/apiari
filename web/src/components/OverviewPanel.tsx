import { ArrowRight, BellRing, BookOpen, Bot, FolderGit2, Sparkles, Wrench } from "lucide-react";
import type { Bot as BotType, Followup, Repo, ResearchTask, Worker } from "../types";
import styles from "./OverviewPanel.module.css";

interface Props {
  workspace: string;
  bots: BotType[];
  workers: Worker[];
  repos: Repo[];
  followups: Followup[];
  researchTasks: ResearchTask[];
  unread: Record<string, number>;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
  onOpenMode: (mode: "chat" | "workers" | "repos" | "docs") => void;
}

function statusLabel(workers: Worker[]) {
  const running = workers.filter((w) => w.status === "running" || w.status === "active").length;
  const review = workers.filter((w) => w.status === "waiting").length;
  if (running && review) return `${running} running · ${review} in review`;
  if (running) return `${running} running`;
  if (review) return `${review} in review`;
  return "No active workers";
}

export function OverviewPanel({
  workspace,
  bots,
  workers,
  repos,
  followups,
  researchTasks,
  unread,
  onSelectBot,
  onSelectWorker,
  onOpenMode,
}: Props) {
  const pendingFollowups = followups.filter((f) => f.status === "pending");
  const runningWorkers = workers.filter((w) => w.status === "running" || w.status === "active" || w.status === "waiting");
  const dirtyRepos = repos.filter((r) => !r.is_clean);
  const activeResearch = researchTasks.filter((task) => task.status === "running");

  return (
    <div className={styles.page}>
      <header className={styles.hero}>
        <div>
          <div className={styles.eyebrow}>Workspace control room</div>
          <h1 className={styles.title}>{workspace}</h1>
          <p className={styles.summary}>
            Make bots, workers, repos, docs, and follow-ups the primary objects. Chat stays available, but the workspace leads.
          </p>
        </div>
        <div className={styles.heroActions}>
          <button className={styles.primaryAction} onClick={() => onSelectBot("Main")}>
            Open Main chat
          </button>
          <button className={styles.secondaryAction} onClick={() => onOpenMode("workers")}>
            Review workers
          </button>
        </div>
      </header>

      <section className={styles.metricGrid}>
        <button className={styles.metricCard} onClick={() => onOpenMode("chat")}>
          <Bot size={18} />
          <span className={styles.metricLabel}>Bots</span>
          <strong className={styles.metricValue}>{bots.length}</strong>
          <span className={styles.metricMeta}>{Object.values(unread).reduce((sum, value) => sum + value, 0)} unread messages</span>
        </button>
        <button className={styles.metricCard} onClick={() => onOpenMode("workers")}>
          <Wrench size={18} />
          <span className={styles.metricLabel}>Workers</span>
          <strong className={styles.metricValue}>{workers.length}</strong>
          <span className={styles.metricMeta}>{statusLabel(workers)}</span>
        </button>
        <button className={styles.metricCard} onClick={() => onOpenMode("repos")}>
          <FolderGit2 size={18} />
          <span className={styles.metricLabel}>Repos</span>
          <strong className={styles.metricValue}>{repos.length}</strong>
          <span className={styles.metricMeta}>{dirtyRepos.length} modified</span>
        </button>
        <button className={styles.metricCard} onClick={() => onOpenMode("docs")}>
          <BookOpen size={18} />
          <span className={styles.metricLabel}>Docs & Follow-ups</span>
          <strong className={styles.metricValue}>{pendingFollowups.length}</strong>
          <span className={styles.metricMeta}>{activeResearch.length} active research tasks</span>
        </button>
      </section>

      <section className={styles.columns}>
        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Immediate queue</div>
              <div className={styles.cardSubtitle}>What likely needs attention next</div>
            </div>
            <BellRing size={16} className={styles.cardIcon} />
          </div>
          {pendingFollowups.length === 0 && activeResearch.length === 0 ? (
            <div className={styles.empty}>No pending follow-ups or running research.</div>
          ) : (
            <div className={styles.list}>
              {pendingFollowups.slice(0, 4).map((followup) => (
                <div key={followup.id} className={styles.listRow}>
                  <div>
                    <div className={styles.rowTitle}>{followup.action}</div>
                    <div className={styles.rowMeta}>{followup.bot} · due {new Date(followup.fires_at).toLocaleString()}</div>
                  </div>
                </div>
              ))}
              {activeResearch.slice(0, 3).map((task) => (
                <div key={task.id} className={styles.listRow}>
                  <div>
                    <div className={styles.rowTitle}>{task.topic}</div>
                    <div className={styles.rowMeta}>Research running</div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Bots</div>
              <div className={styles.cardSubtitle}>Direct entry points into the workspace</div>
            </div>
            <Sparkles size={16} className={styles.cardIcon} />
          </div>
          <div className={styles.list}>
            {bots.map((bot) => (
              <button key={bot.name} className={styles.actionRow} onClick={() => onSelectBot(bot.name)} aria-label={`Open overview bot ${bot.name}`}>
                <div>
                  <div className={styles.rowTitle}>{bot.name}</div>
                  <div className={styles.rowMeta}>{bot.role || bot.provider || "Bot"}</div>
                </div>
                <div className={styles.rowRight}>
                  {unread[bot.name] ? <span className={styles.badge}>{unread[bot.name]} unread</span> : null}
                  <ArrowRight size={14} />
                </div>
              </button>
            ))}
          </div>
        </div>
      </section>

      <section className={styles.columns}>
        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Workers in flight</div>
              <div className={styles.cardSubtitle}>Execution should be visible without opening chat</div>
            </div>
            <Wrench size={16} className={styles.cardIcon} />
          </div>
          {runningWorkers.length === 0 ? (
            <div className={styles.empty}>No running or review-stage workers.</div>
          ) : (
            <div className={styles.list}>
              {runningWorkers.slice(0, 4).map((worker) => (
                <button key={worker.id} className={styles.actionRow} onClick={() => onSelectWorker(worker.id)}>
                  <div>
                    <div className={styles.rowTitle}>{worker.id}</div>
                    <div className={styles.rowMeta}>{worker.status} · {worker.branch.replace(/^swarm\//, "")}</div>
                  </div>
                  <div className={styles.rowRight}>
                    {worker.review_state ? <span className={styles.badge}>{worker.review_state}</span> : null}
                    <ArrowRight size={14} />
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>

        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Repo state</div>
              <div className={styles.cardSubtitle}>Operational health beats hidden sidebars</div>
            </div>
            <FolderGit2 size={16} className={styles.cardIcon} />
          </div>
          {repos.length === 0 ? (
            <div className={styles.empty}>No repos discovered.</div>
          ) : (
            <div className={styles.list}>
              {repos.slice(0, 5).map((repo) => (
                <div key={repo.path} className={styles.listRow}>
                  <div>
                    <div className={styles.rowTitle}>{repo.name}</div>
                    <div className={styles.rowMeta}>{repo.branch} · {repo.workers.length} workers</div>
                  </div>
                  <span className={`${styles.healthBadge} ${repo.is_clean ? styles.clean : styles.modified}`}>
                    {repo.is_clean ? "clean" : "modified"}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
