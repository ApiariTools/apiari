import type { ComponentType } from "react";
import { Bot, FileText, LayoutGrid, MessageSquare, Package, Wrench } from "lucide-react";
import type { Bot as BotType } from "../types";
import styles from "./WorkspaceNav.module.css";

export type WorkspaceMode = "overview" | "chat" | "workers" | "repos" | "docs";

interface ModeItem {
  id: WorkspaceMode;
  label: string;
  count?: number | null;
}

interface Props {
  activeMode: WorkspaceMode;
  onSelectMode: (mode: WorkspaceMode) => void;
  bots: BotType[];
  activeBot: string | null;
  onSelectBot: (name: string) => void;
  unread?: Record<string, number>;
  workerCount: number;
  repoCount: number;
  pendingFollowupCount: number;
  mobileOpen?: boolean;
}

const MODE_ICONS = {
  overview: LayoutGrid,
  chat: MessageSquare,
  workers: Wrench,
  repos: Package,
  docs: FileText,
} satisfies Record<WorkspaceMode, ComponentType<{ size?: number; className?: string }>>;

const BOT_COLORS: Record<string, string> = {
  Main: "var(--accent)",
  Customer: "var(--red)",
  Performance: "var(--green)",
  Social: "var(--blue)",
  Product: "var(--purple)",
};

function botColor(bot: BotType): string {
  return bot.color || BOT_COLORS[bot.name] || "var(--text-faint)";
}

export function WorkspaceNav({
  activeMode,
  onSelectMode,
  bots,
  activeBot,
  onSelectBot,
  unread,
  workerCount,
  repoCount,
  pendingFollowupCount,
  mobileOpen,
}: Props) {
  const modes: ModeItem[] = [
    { id: "overview", label: "Overview", count: pendingFollowupCount > 0 ? pendingFollowupCount : null },
    { id: "chat", label: "Chat", count: null },
    { id: "workers", label: "Workers", count: workerCount || null },
    { id: "repos", label: "Repos", count: repoCount || null },
    { id: "docs", label: "Docs", count: null },
  ];

  return (
    <aside className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
      <div className={styles.sectionLabel}>Workspace</div>
      <div className={styles.modeList}>
        {modes.map((mode) => {
          const Icon = MODE_ICONS[mode.id];
          return (
            <button
              key={mode.id}
              className={`${styles.modeBtn} ${activeMode === mode.id ? styles.active : ""}`}
              onClick={() => onSelectMode(mode.id)}
            >
              <span className={styles.modeLeft}>
                <Icon size={16} className={styles.modeIcon} />
                <span>{mode.label}</span>
              </span>
              {mode.count ? <span className={styles.modeBadge}>{mode.count}</span> : null}
            </button>
          );
        })}
      </div>

      <div className={styles.sectionDivider} />
      <div className={styles.sectionLabel}>Bots</div>
      <div className={styles.botList}>
        {bots.map((bot) => {
          const count = unread?.[bot.name] || 0;
          const isActive = activeMode === "chat" && activeBot === bot.name;
          return (
            <button
              key={bot.name}
              className={`${styles.botBtn} ${isActive ? styles.active : ""}`}
              onClick={() => onSelectBot(bot.name)}
              aria-label={`Open bot ${bot.name}`}
            >
              <span className={styles.botLeft}>
                <span className={styles.botDot} style={{ background: botColor(bot) }} />
                <span className={styles.botMeta}>
                  <span className={styles.botName}>{bot.name}</span>
                  {bot.role ? <span className={styles.botRole}>{bot.role}</span> : null}
                </span>
              </span>
              {count > 0 && !isActive ? <span className={styles.botBadge}>{count}</span> : null}
            </button>
          );
        })}
      </div>

      <div className={styles.tip}>
        <Bot size={14} />
        <span>Chat stays first-class, but workspace state leads the UI.</span>
      </div>
    </aside>
  );
}
