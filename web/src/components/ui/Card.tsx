import { clsx } from "clsx";
import type { HTMLAttributes } from "react";

/**
 * Surface container. Borders and elevation colours carry the hierarchy;
 * shadows are reserved for overlays (docs/09 §11.4).
 */
export function Card({ className, children, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={clsx("rounded-card border border-border bg-surface text-text", className)}
      {...props}
    >
      {children}
    </div>
  );
}

export function CardHeader({ className, children, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={clsx(
        "flex flex-wrap items-start justify-between gap-3 border-b border-border px-5 py-4",
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
}

export interface CardTitleProps extends HTMLAttributes<HTMLHeadingElement> {
  as?: "h1" | "h2" | "h3" | "h4";
}

export function CardTitle({ as = "h2", className, children, ...props }: CardTitleProps) {
  const Heading = as;
  return (
    <Heading className={clsx("text-heading-sm font-semibold text-text", className)} {...props}>
      {children}
    </Heading>
  );
}

export function CardDescription({ className, children, ...props }: HTMLAttributes<HTMLParagraphElement>) {
  return (
    <p className={clsx("mt-1 text-body-sm text-text-secondary", className)} {...props}>
      {children}
    </p>
  );
}

export function CardContent({ className, children, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={clsx("px-5 py-4", className)} {...props}>
      {children}
    </div>
  );
}

export function CardFooter({ className, children, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={clsx(
        "flex flex-wrap items-center justify-between gap-3 border-t border-border px-5 py-3",
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
}
