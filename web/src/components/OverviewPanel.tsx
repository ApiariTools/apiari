import { useEffect, useMemo, useState } from "react";
import { ArrowRight, BellRing, BookOpen, Bot, FolderGit2, Sparkles, Wrench } from "lucide-react";
import { repoSyncLabel } from "../repoSync";
import type { Bot as BotType, Followup, Repo, ResearchTask, Worker } from "../types";
import type { WorkspaceMode } from "../consoleConfig";
import * as api from "../api";
import { ObjectRow } from "../primitives/ObjectRow";
import { StatusBadge } from "../primitives/StatusBadge";
import styles from "./OverviewPanel.module.css";

interface Props {
  workspace: string;
  remote?: string;
  bots: BotType[];
  workers: Worker[];
  repos: Repo[];
  followups: Followup[];
  researchTasks: ResearchTask[];
  unread: Record<string, number>;
  primaryBot: string;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
  onOpenMode: (mode: WorkspaceMode) => void;
  showHero?: boolean;
}

function statusLabel(workers: Worker[]) {
  const running = workers.filter((w) => w.status === "running" || w.status === "active").length;
  const review = workers.filter((w) => w.status === "waiting").length;
  if (running && review) return `${running} running · ${review} in review`;
  if (running) return `${running} running`;
  if (review) return `${review} in review`;
  return "No active workers";
}

function relativeTimeLabel(iso: string) {
  const deltaMinutes = Math.max(1, Math.round((Date.now() - new Date(iso).getTime()) / 60000));
  if (deltaMinutes < 60) return `${deltaMinutes}m`;
  const hours = Math.round(deltaMinutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.round(hours / 24);
  return `${days}d`;
}

function previewLabel(content?: string | null) {
  if (!content) return "Unread conversation";
  return content.length > 84 ? `${content.slice(0, 84).trimEnd()}…` : content;
}

export function OverviewPanel({
  workspace,
  remote,
  bots,
  workers,
  repos,
  followups,
  researchTasks,
  unread,
  primaryBot,
  onSelectBot,
  onSelectWorker,
  onOpenMode,
  showHero = true,
}: Props) {
  const [latestUnreadByBot, setLatestUnreadByBot] = useState<Record<string, { content: string; created_at: string } | null>>({});
  const pendingFollowups = followups.filter((f) => f.status === "pending");
  const runningWorkers = workers.filter((w) => w.status === "running" || w.status === "active" || w.status === "waiting");
  const dirtyRepos = repos.filter((r) => !r.is_clean);
  const activeResearch = researchTasks.filter((task) => task.status === "running");
  const unreadBots = useMemo(() => bots
    .filter((bot) => (unread[bot.name] ?? 0) > 0)
    .sort((a, b) => (unread[b.name] ?? 0) - (unread[a.name] ?? 0)), [bots, unread]);
  const nextWorker = runningWorkers[0];
  const nextFollowup = pendingFollowups.slice().sort((a, b) => a.fires_at.localeCompare(b.fires_at))[0];
  const continueBot = [...unreadBots]
    .filter((bot) => latestUnreadByBot[bot.name]?.created_at)
    .sort((a, b) => (latestUnreadByBot[b.name]?.created_at ?? "").localeCompare(latestUnreadByBot[a.name]?.created_at ?? ""))[0]
    ?? unreadBots[0]
    ?? bots.find((bot) => bot.name === primaryBot)
    ?? bots[0];

  useEffect(() => {
    let cancelled = false;
    const targets = Array.from(new Set([continueBot?.name, ...unreadBots.map((bot) => bot.name)].filter(Boolean))) as string[];
    if (targets.length === 0) {
      setLatestUnreadByBot({});
      return () => {
        cancelled = true;
      };
    }

    Promise.all(
      targets.map(async (botName) => {
        try {
          const messages = await api.getConversations(workspace, botName, 1, remote);
          const latest = messages[messages.length - 1];
          return [botName, latest ? { content: latest.content, created_at: latest.created_at } : null] as const;
        } catch {
          return [botName, null] as const;
        }
      }),
    ).then((entries) => {
      if (!cancelled) {
        setLatestUnreadByBot(Object.fromEntries(entries));
      }
    });

    return () => {
      cancelled = true;
    };
  }, [workspace, remote, continueBot?.name, unreadBots]);

  const continueMessage = continueBot ? latestUnreadByBot[continueBot.name] : null;

  const continueActions = [
    continueBot
      ? {
          key: `bot-${continueBot.name}`,
          title: continueMessage ? previewLabel(continueMessage.content) : `Continue ${continueBot.name}`,
          meta: continueMessage
            ? `${continueBot.name} · ${relativeTimeLabel(continueMessage.created_at)}`
            : `${continueBot.name}${unread[continueBot.name] ? ` · ${unread[continueBot.name]} unread` : ""}${continueBot.role ? ` · ${continueBot.role}` : ""}`,
          action: () => onSelectBot(continueBot.name),
        }
      : null,
    nextWorker
      ? {
          key: `worker-${nextWorker.id}`,
          title: `Review ${nextWorker.id}`,
          meta: `${nextWorker.status} · ${nextWorker.branch.replace(/^swarm\//, "")}`,
          action: () => onSelectWorker(nextWorker.id),
        }
      : null,
    nextFollowup
      ? {
          key: `followup-${nextFollowup.id}`,
          title: nextFollowup.action,
          meta: `${nextFollowup.bot} · due in ${relativeTimeLabel(nextFollowup.fires_at)}`,
          action: () => onSelectBot(nextFollowup.bot),
        }
      : null,
  ].filter(Boolean) as Array<{ key: string; title: string; meta: string; action: () => void }>;

  return (
    <div className={styles.page}>
      {showHero ? (
        <header className={styles.hero}>
          <div>
            <div className={styles.eyebrow}>Workspace control room</div>
            <h1 className={styles.title}>{workspace}</h1>
            <p className={styles.summary}>
              Make the next action obvious: continue a conversation, review worker output, or start a fresh task.
            </p>
          </div>
          <div className={styles.heroActions}>
            <button className={styles.primaryAction} onClick={() => continueBot && onSelectBot(continueBot.name)}>
              {continueBot ? `Continue ${continueBot.name}` : "Open chat"}
            </button>
            <button className={styles.secondaryAction} onClick={() => onOpenMode("workers")}>
              Review workers
            </button>
          </div>
        </header>
      ) : null}

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
        <button className={styles.metricCard} onClick={() => onOpenMode("docs")}>
          <BookOpen size={18} />
          <span className={styles.metricLabel}>Docs</span>
          <strong className={styles.metricValue}>{pendingFollowups.length}</strong>
          <span className={styles.metricMeta}>{activeResearch.length} active research tasks</span>
        </button>
        <button className={styles.metricCard} onClick={() => onOpenMode("repos")}>
          <FolderGit2 size={18} />
          <span className={styles.metricLabel}>Repos</span>
          <strong className={styles.metricValue}>{repos.length}</strong>
          <span className={styles.metricMeta}>{dirtyRepos.length} modified</span>
        </button>
      </section>

      <section className={styles.columns}>
        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Needs attention</div>
              <div className={styles.cardSubtitle}>Unread chats, due follow-ups, and active work</div>
            </div>
            <BellRing size={16} className={styles.cardIcon} />
          </div>
          {unreadBots.length === 0 && pendingFollowups.length === 0 && runningWorkers.length === 0 ? (
            <div className={styles.empty}>Nothing urgent right now.</div>
          ) : (
            <div className={styles.list}>
              {unreadBots.slice(0, 3).map((bot) => (
                <ObjectRow
                  key={`unread-${bot.name}`}
                  onClick={() => onSelectBot(bot.name)}
                  ariaLabel={`Open unread bot ${bot.name}`}
                  title={previewLabel(latestUnreadByBot[bot.name]?.content)}
                  meta={`${bot.name} · ${unread[bot.name]} unread${latestUnreadByBot[bot.name]?.created_at ? ` · ${relativeTimeLabel(latestUnreadByBot[bot.name]!.created_at)}` : ""}`}
                  right={<StatusBadge tone="accent">unread</StatusBadge>}
                />
              ))}
              {pendingFollowups.slice(0, 2).map((followup) => (
                <ObjectRow
                  key={followup.id}
                  onClick={() => onSelectBot(followup.bot)}
                  title={followup.action}
                  meta={`${followup.bot} · due ${relativeTimeLabel(followup.fires_at)}`}
                  right={<StatusBadge tone="accent">follow-up</StatusBadge>}
                />
              ))}
              {runningWorkers.slice(0, 2).map((worker) => (
                <ObjectRow
                  key={`worker-${worker.id}`}
                  onClick={() => onSelectWorker(worker.id)}
                  title={worker.id}
                  meta={`${worker.status} · ${worker.branch.replace(/^swarm\//, "")}`}
                  right={<StatusBadge tone="success">worker</StatusBadge>}
                />
              ))}
            </div>
          )}
        </div>

        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Continue</div>
              <div className={styles.cardSubtitle}>The most likely next action</div>
            </div>
            <ArrowRight size={16} className={styles.cardIcon} />
          </div>
          {continueActions.length === 0 ? (
            <div className={styles.empty}>No active chat, worker, or follow-up to resume.</div>
          ) : (
            <div className={styles.list}>
              {continueActions.map((entry) => (
                <ObjectRow
                  key={entry.key}
                  title={entry.title}
                  meta={entry.meta}
                  onClick={entry.action}
                  right={<ArrowRight size={14} />}
                />
              ))}
            </div>
          )}
        </div>
      </section>

      <section className={styles.columns}>
        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Start something</div>
              <div className={styles.cardSubtitle}>Best entry points for new work</div>
            </div>
            <Sparkles size={16} className={styles.cardIcon} />
          </div>
          <div className={styles.list}>
            {bots.map((bot) => (
              <ObjectRow
                key={bot.name}
                onClick={() => onSelectBot(bot.name)}
                ariaLabel={`Start with bot ${bot.name}`}
                title={bot.name}
                meta={bot.description || bot.role || bot.provider || "Bot"}
                right={<ArrowRight size={14} />}
              />
            ))}
          </div>
        </div>

        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Workers in flight</div>
              <div className={styles.cardSubtitle}>Execution status without digging</div>
            </div>
            <Wrench size={16} className={styles.cardIcon} />
          </div>
          {runningWorkers.length === 0 ? (
            <div className={styles.empty}>No running or review-stage workers.</div>
          ) : (
            <div className={styles.list}>
              {runningWorkers.slice(0, 4).map((worker) => (
                <ObjectRow
                  key={worker.id}
                  onClick={() => onSelectWorker(worker.id)}
                  title={worker.id}
                  meta={`${worker.status} · ${worker.branch.replace(/^swarm\//, "")}`}
                  right={(
                    <>
                      {worker.review_state ? <StatusBadge tone="accent">{worker.review_state}</StatusBadge> : null}
                      <ArrowRight size={14} />
                    </>
                  )}
                />
              ))}
            </div>
          )}
        </div>
      </section>

      <section className={styles.columns}>
        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <div>
              <div className={styles.cardTitle}>Repo state</div>
              <div className={styles.cardSubtitle}>Secondary context, not the main call to action</div>
            </div>
            <FolderGit2 size={16} className={styles.cardIcon} />
          </div>
          {repos.length === 0 ? (
            <div className={styles.empty}>No repos discovered.</div>
          ) : (
            <div className={styles.list}>
              {repos.slice(0, 5).map((repo) => (
                <ObjectRow
                  key={repo.path}
                  title={repo.name}
                  meta={`${repo.branch} · ${repo.is_clean ? "clean" : "modified"} · ${repoSyncLabel(repo)} · ${repo.workers.length} workers`}
                  right={(
                    <StatusBadge tone={repo.is_clean ? "success" : "accent"}>
                      {repo.is_clean ? "clean" : "modified"}
                    </StatusBadge>
                  )}
                />
              ))}
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
