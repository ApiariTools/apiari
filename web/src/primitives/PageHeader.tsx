import type { ReactNode } from "react";
import styles from "./PageHeader.module.css";

interface Action {
  label: string;
  onClick: () => void;
  kind?: "primary" | "secondary";
}

interface Props {
  eyebrow?: string;
  title: string;
  summary?: string;
  actions?: Action[];
  meta?: ReactNode;
}

export function PageHeader({ eyebrow, title, summary, actions = [], meta }: Props) {
  return (
    <div className={styles.wrap}>
      <div>
        {eyebrow ? <div className={styles.eyebrow}>{eyebrow}</div> : null}
        <h1 className={styles.title}>{title}</h1>
        {summary ? <p className={styles.summary}>{summary}</p> : null}
        {meta}
      </div>
      {actions.length > 0 ? (
        <div className={styles.actions}>
          {actions.map((action) => (
            <button
              key={action.label}
              className={action.kind === "primary" ? styles.primary : styles.secondary}
              onClick={action.onClick}
            >
              {action.label}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
