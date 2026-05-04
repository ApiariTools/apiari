import type { ReactNode } from "react";
import styles from "./ToolPanel.module.css";

interface Props {
  title: string;
  subtitle?: string;
  children: ReactNode;
  mobileOpen?: boolean;
  onClose?: () => void;
}

export function ToolPanel({ title, subtitle, children, mobileOpen = false, onClose }: Props) {
  return (
    <>
      {mobileOpen ? <div className={styles.backdrop} onClick={onClose} /> : null}
      <section className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
        <div className={styles.header}>
          <div className={styles.title}>{title}</div>
          {subtitle ? <div className={styles.subtitle}>{subtitle}</div> : null}
        </div>
        <div className={styles.content}>{children}</div>
      </section>
    </>
  );
}
