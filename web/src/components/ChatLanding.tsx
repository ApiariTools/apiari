import { useEffect, useMemo, useState } from "react";
import * as api from "@apiari/api";
import type { Bot, Message } from "@apiari/types";
import styles from "./ChatLanding.module.css";

interface Props {
  workspace: string;
  remote?: string;
  bots: Bot[];
  unread: Record<string, number>;
  onSelectBot: (name: string) => void;
}

function formatMessagePreview(message: Message | null) {
  if (!message) return { label: "No recent conversation", content: "" };
  return {
    label: message.role === "assistant" ? "Last reply" : "Last note",
    content: message.content,
  };
}

function pickPreviewMessage(messages: Message[]) {
  if (messages.length === 0) return null;
  const latestUserOrSystem = [...messages]
    .reverse()
    .find((message) => message.role !== "assistant" && message.content.trim().length > 0);
  return latestUserOrSystem ?? messages[messages.length - 1] ?? null;
}

function formatMessageTime(value?: string) {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function recentTimestamp(message: Message | null) {
  if (!message?.created_at) return "";
  return message.created_at;
}

export function ChatLanding({ workspace, remote, bots, unread, onSelectBot }: Props) {
  const [recentByBot, setRecentByBot] = useState<
    Record<string, { latest: Message | null; preview: Message | null }>
  >({});

  useEffect(() => {
    let cancelled = false;
    if (!workspace || bots.length === 0) {
      setRecentByBot({});
      return () => {
        cancelled = true;
      };
    }

    Promise.all(
      bots.map(async (bot) => {
        try {
          const messages = await api.getConversations(workspace, bot.name, 3, remote);
          return [
            bot.name,
            {
              latest: messages[messages.length - 1] ?? null,
              preview: pickPreviewMessage(messages),
            },
          ] as const;
        } catch {
          return [bot.name, { latest: null, preview: null }] as const;
        }
      }),
    ).then((entries) => {
      if (!cancelled) setRecentByBot(Object.fromEntries(entries));
    });

    return () => {
      cancelled = true;
    };
  }, [workspace, remote, bots]);

  const rankedBots = useMemo(() => {
    return [...bots].sort((a, b) => {
      const recentDelta = recentTimestamp(recentByBot[b.name]?.latest ?? null).localeCompare(
        recentTimestamp(recentByBot[a.name]?.latest ?? null),
      );
      if (recentDelta !== 0) return recentDelta;
      const unreadDelta = (unread[b.name] ?? 0) - (unread[a.name] ?? 0);
      if (unreadDelta !== 0) return unreadDelta;
      return a.name.localeCompare(b.name);
    });
  }, [bots, unread, recentByBot]);

  const featuredBot = rankedBots[0] ?? null;
  const attentionBots = [...bots]
    .filter((bot) => bot !== featuredBot && (unread[bot.name] ?? 0) > 0)
    .sort((a, b) => {
      const unreadDelta = (unread[b.name] ?? 0) - (unread[a.name] ?? 0);
      if (unreadDelta !== 0) return unreadDelta;
      return recentTimestamp(recentByBot[b.name]?.latest ?? null).localeCompare(
        recentTimestamp(recentByBot[a.name]?.latest ?? null),
      );
    });
  const remainingBots = rankedBots.filter(
    (bot) => bot !== featuredBot && !attentionBots.includes(bot),
  );

  const renderMetaChips = (bot: Bot) => {
    const meta = [bot.provider, bot.model].filter(Boolean).join(" / ");
    const watchSummary = bot.watch.length > 0 ? bot.watch.join(", ") : "General workspace context";
    return (
      <div className={styles.contextRow}>
        <span className={styles.contextChip}>{meta || "Default runtime"}</span>
        <span className={styles.contextChip}>{watchSummary}</span>
      </div>
    );
  };

  const renderRow = (bot: Bot) => {
    const unreadCount = unread[bot.name] ?? 0;
    const recentEntry = recentByBot[bot.name] ?? { latest: null, preview: null };
    const recentPreview = formatMessagePreview(recentEntry.preview);
    const recentTime = formatMessageTime(recentEntry.latest?.created_at);
    return (
      <button
        key={bot.name}
        type="button"
        className={styles.row}
        onClick={() => onSelectBot(bot.name)}
        aria-label={`Open bot ${bot.name}`}
      >
        <div className={styles.rowTop}>
          <div className={styles.identity}>
            <span
              className={styles.color}
              style={bot.color ? { background: bot.color } : undefined}
              aria-hidden="true"
            />
            <div className={styles.identityText}>
              <span className={styles.name}>{bot.name}</span>
              {bot.role ? <span className={styles.role}>{bot.role}</span> : null}
            </div>
          </div>
          <div className={styles.rowMeta}>
            {recentTime ? <span className={styles.time}>{recentTime}</span> : null}
            {unreadCount > 0 ? <span className={styles.badge}>{unreadCount} unread</span> : null}
          </div>
        </div>
        <div className={styles.rowPreview}>
          <span className={styles.previewLabel}>{recentPreview.label}</span>
          {recentPreview.content ? <p className={styles.preview}>{recentPreview.content}</p> : null}
        </div>
        {renderMetaChips(bot)}
      </button>
    );
  };

  return (
    <section className={styles.landing}>
      <div className={styles.header}>
        <span className={styles.kicker}>Chat workspace</span>
        <h2 className={styles.title}>Choose a bot</h2>
        <p className={styles.copy}>
          Continue where work is already active, or start a new conversation with the bot best
          suited for the job.
        </p>
      </div>

      {featuredBot ? (
        <button
          type="button"
          className={styles.featured}
          onClick={() => onSelectBot(featuredBot.name)}
          aria-label={`Open bot ${featuredBot.name}`}
        >
          <div className={styles.featuredTop}>
            <div>
              <div className={styles.sectionTitle}>Start here</div>
              <div className={styles.featuredTitle}>Open {featuredBot.name}</div>
            </div>
            {(unread[featuredBot.name] ?? 0) > 0 ? (
              <span className={styles.badge}>{unread[featuredBot.name]} unread</span>
            ) : null}
          </div>
          {featuredBot.description ? (
            <p className={styles.description}>{featuredBot.description}</p>
          ) : null}
          {renderMetaChips(featuredBot)}
          {(() => {
            const recentEntry = recentByBot[featuredBot.name] ?? { latest: null, preview: null };
            const recentPreview = formatMessagePreview(recentEntry.preview);
            const recentTime = formatMessageTime(recentEntry.latest?.created_at);
            return (
              <div className={styles.previewBlock}>
                <div className={styles.previewHeader}>
                  <span className={styles.previewLabel}>{recentPreview.label}</span>
                  {recentTime ? <span className={styles.time}>{recentTime}</span> : null}
                </div>
                {recentPreview.content ? (
                  <p className={styles.preview}>{recentPreview.content}</p>
                ) : null}
              </div>
            );
          })()}
        </button>
      ) : null}

      {attentionBots.length > 0 ? (
        <div className={styles.section}>
          <div className={styles.sectionTitle}>Needs attention</div>
          <div className={styles.list}>{attentionBots.map(renderRow)}</div>
        </div>
      ) : null}

      {remainingBots.length > 0 ? (
        <div className={styles.section}>
          <div className={styles.sectionTitle}>All bots</div>
          <div className={styles.list}>{remainingBots.map(renderRow)}</div>
        </div>
      ) : null}
    </section>
  );
}
