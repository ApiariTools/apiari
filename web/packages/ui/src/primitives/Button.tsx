import { forwardRef } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";
import styles from "./Button.module.css";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "primary" | "secondary" | "ghost" | "danger" | "icon";
  size?: "sm" | "md" | "lg";
  loading?: boolean;
  children?: ReactNode;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", loading = false, disabled, children, className, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      className={[styles.btn, styles[variant], styles[size], className].filter(Boolean).join(" ")}
      disabled={disabled || loading}
      {...rest}
    >
      {loading && <span className={styles.spinner} aria-hidden="true" />}
      {children}
    </button>
  );
});
