import { clsx } from "clsx";
import { Loader2 } from "lucide-react";
import { forwardRef, type ButtonHTMLAttributes } from "react";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md";

const VARIANT_CLASSES: Record<ButtonVariant, string> = {
  primary: "border-accent bg-accent text-on-accent hover:bg-accent/90",
  secondary: "border-border-strong bg-raised text-text hover:border-accent hover:text-accent",
  ghost: "border-transparent bg-transparent text-text-secondary hover:bg-raised hover:text-text",
  danger: "border-danger bg-danger text-on-danger hover:bg-danger/90",
};

// docs/09 §11.6: keep a 44px touch floor, and only shrink to 36px on fine
// (mouse-like) pointers, where precision is high and space is tight.
const SIZE_CLASSES: Record<ButtonSize, string> = {
  sm: "min-h-11 pointer-fine:min-h-9 gap-1.5 px-3 text-caption",
  md: "min-h-11 gap-2 px-4 text-body",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Disables the button and shows a spinner while an action is in flight. */
  pending?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "primary", size = "md", pending = false, disabled, className, children, ...props },
  ref,
) {
  return (
    <button
      ref={ref}
      type="button"
      className={clsx(
        "ui-button inline-flex items-center justify-center rounded-card border font-medium transition-colors duration-150 ease-out",
        "disabled:pointer-events-none disabled:opacity-50",
        VARIANT_CLASSES[variant],
        SIZE_CLASSES[size],
        className,
      )}
      disabled={disabled || pending}
      aria-busy={pending || undefined}
      {...props}
    >
      {pending ? <Loader2 aria-hidden="true" className="size-4 animate-spin" /> : null}
      {children}
    </button>
  );
});
