import { clsx } from "clsx";
import { forwardRef, useId, type InputHTMLAttributes, type ReactNode } from "react";

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  /** Visible (or screen-reader-only) label, always associated via `htmlFor`. */
  label: string;
  hideLabel?: boolean;
  hint?: ReactNode;
  error?: string;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  {
    label,
    hideLabel = false,
    hint,
    error,
    id,
    className,
    "aria-describedby": ariaDescribedBy,
    ...props
  },
  ref,
) {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const hintId = hint ? `${inputId}-hint` : undefined;
  const errorId = error ? `${inputId}-error` : undefined;
  const describedBy = [ariaDescribedBy, hintId, errorId].filter(Boolean).join(" ");

  return (
    <div className="flex w-full flex-col gap-1.5">
      <label
        htmlFor={inputId}
        className={clsx(
          "text-caption font-medium text-text-secondary",
          hideLabel && "sr-only",
        )}
      >
        {label}
      </label>
      <input
        ref={ref}
        id={inputId}
        {...props}
        className={clsx(
          "min-h-11 w-full rounded-card border bg-surface px-3 py-2 text-body text-text",
          "placeholder:text-text-secondary transition-colors duration-150 ease-out",
          "disabled:cursor-not-allowed disabled:opacity-60",
          error
            ? "border-danger"
            : "border-border hover:border-border-strong focus-visible:border-accent",
          className,
        )}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy || undefined}
      />
      {hint ? (
        <p id={hintId} className="text-caption text-text-secondary">
          {hint}
        </p>
      ) : null}
      {error ? (
        <p id={errorId} role="alert" className="text-caption font-medium text-danger">
          {error}
        </p>
      ) : null}
    </div>
  );
});
