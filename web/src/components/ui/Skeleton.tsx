import { clsx } from "clsx";
import type { HTMLAttributes } from "react";

export interface SkeletonProps extends HTMLAttributes<HTMLDivElement> {
  /** Optional visible shape helpers; sizing is controlled by `className`. */
  rounded?: "card" | "full";
}

/**
 * Loading placeholder. It is hidden from assistive technology; the parent
 * should expose the loading state (for example with `aria-busy` or a status
 * message) so screen readers are not spammed per block.
 */
export function Skeleton({ rounded = "card", className, ...props }: SkeletonProps) {
  return (
    <div
      aria-hidden="true"
      className={clsx(
        "animate-pulse bg-raised",
        rounded === "full" ? "rounded-full" : "rounded-card",
        className,
      )}
      {...props}
    />
  );
}
