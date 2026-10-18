import { clsx } from "clsx";
import type { HTMLAttributes } from "react";

export type BadgeVariant = "neutral" | "accent" | "info" | "success" | "warning" | "danger";

/*
 * Labels use primary (secondary for the neutral variant) text so they clear
 * WCAG AA on the 10% tint. The semantic hue stays visible as the dot, border
 * and background instead of being reused as small text colour.
 */
const VARIANT_CLASSES: Record<BadgeVariant, string> = {
  neutral: "border-border bg-raised text-text-secondary",
  accent: "border-accent/40 bg-accent/10 text-text",
  info: "border-info/40 bg-info/10 text-text",
  success: "border-success/40 bg-success/10 text-text",
  warning: "border-warning/40 bg-warning/10 text-text",
  danger: "border-danger/40 bg-danger/10 text-text",
};

const VARIANT_DOT_CLASSES: Record<BadgeVariant, string | null> = {
  neutral: null,
  accent: "bg-accent",
  info: "bg-info",
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-danger",
};

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  variant?: BadgeVariant;
}

/** Small non-interactive label for counts and secondary metadata. */
export function Badge({ variant = "neutral", className, children, ...props }: BadgeProps) {
  const dotClass = VARIANT_DOT_CLASSES[variant];
  return (
    <span
      className={clsx(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-caption font-medium whitespace-nowrap",
        VARIANT_CLASSES[variant],
        className,
      )}
      {...props}
    >
      {dotClass ? (
        <span aria-hidden="true" className={clsx("size-1.5 shrink-0 rounded-full", dotClass)} />
      ) : null}
      {children}
    </span>
  );
}
