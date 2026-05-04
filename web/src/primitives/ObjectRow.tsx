import type { ReactNode } from "react";
import styles from "./ObjectRow.module.css";

interface Props {
  title: ReactNode;
  meta?: ReactNode;
  right?: ReactNode;
  onClick?: () => void;
  ariaLabel?: string;
}

export function ObjectRow({ title, meta, right, onClick, ariaLabel }: Props) {
  const content = (
    <>
      <div className={styles.left}>
        <div className={styles.title}>{title}</div>
        {meta ? <div className={styles.meta}>{meta}</div> : null}
      </div>
      {right ? <div className={styles.right}>{right}</div> : null}
    </>
  );

  if (onClick) {
    return (
      <button className={styles.action} onClick={onClick} aria-label={ariaLabel}>
        {content}
      </button>
    );
  }

  return <div className={styles.row}>{content}</div>;
}
