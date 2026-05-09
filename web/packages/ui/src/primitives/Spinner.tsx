import type { CSSProperties } from "react";
import styles from "./Spinner.module.css";

type Size = "sm" | "md" | "lg";

export interface SpinnerProps {
  size?: Size;
  className?: string;
  style?: CSSProperties;
}

export function Spinner({ size = "md", className, style }: SpinnerProps) {
  return (
    <span
      className={[styles.spinner, styles[size], className].filter(Boolean).join(" ")}
      style={style}
      aria-label="Loading"
      role="status"
    />
  );
}

export interface DotsProps {
  className?: string;
  style?: CSSProperties;
}

export function Dots({ className, style }: DotsProps) {
  return (
    <span
      className={[styles.dots, className].filter(Boolean).join(" ")}
      style={style}
      aria-label="Loading"
      role="status"
    >
      <span />
      <span />
      <span />
    </span>
  );
}

export interface SkeletonProps {
  width?: string | number;
  height?: string | number;
  className?: string;
  style?: CSSProperties;
}

export function Skeleton({ width, height = 14, className, style }: SkeletonProps) {
  return (
    <span
      className={[styles.skeleton, className].filter(Boolean).join(" ")}
      style={{ width, height, ...style }}
      aria-hidden="true"
    />
  );
}
