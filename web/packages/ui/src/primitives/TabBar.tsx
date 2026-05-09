import styles from "./TabBar.module.css";

export interface TabItem {
  value: string;
  label: string;
  badge?: number;
}

export interface TabBarProps {
  tabs: TabItem[];
  value: string;
  onChange: (value: string) => void;
  variant?: "pill" | "underline";
  className?: string;
}

export function TabBar({ tabs, value, onChange, variant = "pill", className }: TabBarProps) {
  return (
    <div
      className={[styles.bar, styles[variant], className].filter(Boolean).join(" ")}
      role="tablist"
    >
      {tabs.map((tab) => (
        <button
          key={tab.value}
          type="button"
          role="tab"
          aria-selected={tab.value === value}
          className={[styles.tab, tab.value === value ? styles.tabActive : ""]
            .filter(Boolean)
            .join(" ")}
          onClick={() => onChange(tab.value)}
        >
          {tab.label}
          {tab.badge != null && tab.badge > 0 && <span className={styles.badge}>{tab.badge}</span>}
        </button>
      ))}
    </div>
  );
}
