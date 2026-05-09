import type { ReactNode } from "react";
import styles from "./StatusBadge.module.css";

type Tone = "accent" | "success" | "danger" | "neutral";

interface Props {
  children: ReactNode;
  tone?: Tone;
}

export function StatusBadge({ children, tone = "neutral" }: Props) {
  return <span className={`${styles.badge} ${styles[tone]}`}>{children}</span>;
}
