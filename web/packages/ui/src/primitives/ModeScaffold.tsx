import type { ReactNode } from "react";
import styles from "./ModeScaffold.module.css";

interface Props {
  header?: ReactNode;
  children: ReactNode;
  scrollBody?: boolean;
  hideHeaderOnMobile?: boolean;
}

export function ModeScaffold({ header, children, scrollBody = false, hideHeaderOnMobile = false }: Props) {
  return (
    <section className={styles.shell}>
      {header ? <div className={`${styles.header} ${hideHeaderOnMobile ? styles.headerHiddenMobile : ""}`}>{header}</div> : null}
      <div className={`${styles.body} ${scrollBody ? styles.bodyScroll : ""}`}>{children}</div>
    </section>
  );
}
