import type { ReactNode } from "react";
import styles from "./EmptyState.module.css";
import { HtmlComment } from "../components/HtmlComment";

interface Props {
  title: string;
  body?: ReactNode;
}

export function EmptyState({ title, body }: Props) {
  return (
    <div className={styles.state}>
      <div className={styles.title}>{title}</div>
      {body ? <div className={styles.body}>{body}</div> : null}
      <HtmlComment text=" apiari-test " />
    </div>
  );
}
