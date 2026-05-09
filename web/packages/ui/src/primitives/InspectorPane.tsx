import type { ReactNode } from "react";
import styles from "./InspectorPane.module.css";

interface Props {
  children?: ReactNode;
  placeholder?: ReactNode;
}

export function InspectorPane({ children, placeholder }: Props) {
  return (
    <aside className={styles.pane}>
      {children ? children : <div className={styles.placeholder}>{placeholder}</div>}
    </aside>
  );
}
