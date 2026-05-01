import { useEffect, useRef, useCallback } from "react";
import { Search, Smartphone } from "lucide-react";
import type { Workspace } from "../types";
import type { UsageData } from "../api";
import styles from "./TopBar.module.css";

interface Props {
  workspaces: Workspace[];
  active: string;
  activeRemote?: string;
  onSelect: (name: string, remote?: string) => void;
  onMenuToggle?: () => void;
  onOpenPalette?: () => void;
  onToggleSimulator?: () => void;
  usage?: UsageData;
}

function dotColor(p: { status: string; usage_percent: number | null }): string {
  if (p.status === "rate_limited") return "var(--red)";
  if (p.status === "error") return "var(--text-faint)";
  const pct = p.usage_percent ?? 0;
  if (pct > 80) return "var(--red)";
  if (pct > 50) return "var(--accent)";
  return "var(--green)";
}

function dotTitle(p: { name: string; status: string; usage_percent: number | null; remaining: string | null; resets_at: string | null }): string {
  let t = `${p.name}: ${p.usage_percent != null ? `${Math.round(p.usage_percent)}% used` : p.status}`;
  if (p.remaining) t += ` — ${p.remaining} remaining`;
  if (p.resets_at) t += ` — resets ${p.resets_at}`;
  return t;
}

export function TopBar({ workspaces, active, activeRemote, onSelect, onMenuToggle, onOpenPalette, onToggleSimulator, usage }: Props) {
  const showDots = usage?.installed && usage.providers.length > 0;

  const activeTabRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    activeTabRef.current?.scrollIntoView({ behavior: "smooth", inline: "center", block: "nearest" });
  }, [active, activeRemote]);

  const setActiveRef = useCallback((el: HTMLButtonElement | null, isActive: boolean) => {
    if (isActive) activeTabRef.current = el;
  }, []);

  return (
    <div className={styles.bar}>
      <button className={styles.hamburger} onClick={onMenuToggle}>
        <span /><span /><span />
      </button>
      <div className={styles.logo}>hive</div>
      <div className={styles.tabScroll}>
        {workspaces.map((ws) => {
          const isActive = ws.name === active && ws.remote === activeRemote;
          return (
            <button
              key={`${ws.remote || "local"}/${ws.name}`}
              ref={(el) => setActiveRef(el, isActive)}
              className={`${styles.tab} ${isActive ? styles.active : ""}`}
              onClick={() => onSelect(ws.name, ws.remote)}
            >
              {ws.name}
              {ws.remote && <span className={styles.remoteBadge}>{ws.remote}</span>}
            </button>
          );
        })}
      </div>
      {showDots && (
        <div className={styles.usageDots}>
          {usage.providers.map((p) => (
            <span
              key={p.name}
              className={styles.usageDot}
              style={{ background: dotColor(p) }}
              title={dotTitle(p)}
              role="img"
              aria-label={dotTitle(p)}
            />
          ))}
        </div>
      )}

      <button
        className={styles.searchBtn}
        onClick={() => onToggleSimulator?.()}
        aria-label="Toggle simulator"
      >
        <Smartphone size={16} />
      </button>
      <button
        className={styles.searchBtn}
        onClick={() => onOpenPalette?.()}
        aria-label="Open command palette"
      >
        <Search size={16} />
      </button>
    </div>
  );
}
