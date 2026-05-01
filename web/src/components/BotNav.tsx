import { FileText } from "lucide-react";
import type { Bot, Worker } from "../types";
import styles from "./BotNav.module.css";

interface Props {
  bots: Bot[];
  workers: Worker[];
  activeBot: string | null;
  activeWorkerId: string | null;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
  mobileOpen?: boolean;
  unread?: Record<string, number>;
  docsOpen?: boolean;
  onSelectDocs?: () => void;
}

const BOT_COLORS: Record<string, string> = {
  Main: "var(--accent)",
  Customer: "var(--red)",
  Performance: "var(--green)",
  Social: "var(--blue)",
  Product: "var(--purple)",
};

function botColor(bot: Bot): string {
  return bot.color || BOT_COLORS[bot.name] || "var(--text-faint)";
}

export function BotNav({
  bots,
  activeBot,
  onSelectBot,
  mobileOpen,
  unread,
  docsOpen,
  onSelectDocs,
}: Props) {
  return (
    <div className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
      <div className={styles.label}>Bots</div>
      {bots.map((b) => {
        const count = unread?.[b.name] || 0;
        return (
          <button
            key={b.name}
            className={`${styles.botBtn} ${activeBot === b.name ? styles.active : ""}`}
            onClick={() => onSelectBot(b.name)}
          >
            <span
              className={styles.dot}
              style={{ background: botColor(b) }}
            />
            <span className={styles.nameGroup}>
              <span className={styles.name}>{b.name}</span>
              {b.role && <span className={styles.role}>{b.role}</span>}
            </span>
            {count > 0 && activeBot !== b.name && (
              <span className={styles.badge}>{count}</span>
            )}
          </button>
        );
      })}
      {onSelectDocs && (
        <>
          <div className={styles.divider} />
          <button
            className={`${styles.botBtn} ${docsOpen ? styles.active : ""}`}
            onClick={onSelectDocs}
          >
            <FileText size={16} style={{ color: docsOpen ? "var(--accent)" : "var(--text-faint)", flexShrink: 0 }} />
            <span className={styles.nameGroup}>
              <span className={styles.name}>Docs</span>
            </span>
          </button>
        </>
      )}
    </div>
  );
}
