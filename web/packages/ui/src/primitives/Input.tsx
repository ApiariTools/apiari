import { forwardRef } from "react";
import type {
  InputHTMLAttributes,
  TextareaHTMLAttributes,
  SelectHTMLAttributes,
  ReactNode,
} from "react";
import styles from "./Input.module.css";

type Size = "sm" | "md" | "lg";

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  size?: Size;
  prefix?: ReactNode;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { size = "md", prefix, className, ...rest },
  ref,
) {
  return (
    <div className={styles.wrap}>
      {prefix && <span className={styles.prefix}>{prefix}</span>}
      <input
        ref={ref}
        className={[styles.input, styles[size], prefix ? styles.hasPrefix : "", className]
          .filter(Boolean)
          .join(" ")}
        {...rest}
      />
    </div>
  );
});

export type TextareaProps = TextareaHTMLAttributes<HTMLTextAreaElement>;

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaProps>(function Textarea(
  { className, ...rest },
  ref,
) {
  return (
    <textarea
      ref={ref}
      className={[styles.textarea, className].filter(Boolean).join(" ")}
      {...rest}
    />
  );
});

export interface SelectProps extends SelectHTMLAttributes<HTMLSelectElement> {
  size?: Size;
}

export const Select = forwardRef<HTMLSelectElement, SelectProps>(function Select(
  { size = "md", className, ...rest },
  ref,
) {
  return (
    <select
      ref={ref}
      className={[styles.select, styles[size], className].filter(Boolean).join(" ")}
      {...rest}
    />
  );
});
