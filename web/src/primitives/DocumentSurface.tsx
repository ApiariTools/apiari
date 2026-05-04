import type { ReactNode } from "react";
import styles from "./DocumentSurface.module.css";

interface Props {
  sidebar: ReactNode;
  editor: ReactNode;
}

export function DocumentSurface({ sidebar, editor }: Props) {
  return (
    <div className={styles.layout}>
      <div className={styles.sidebar}>{sidebar}</div>
      <div className={styles.editor}>{editor}</div>
    </div>
  );
}
