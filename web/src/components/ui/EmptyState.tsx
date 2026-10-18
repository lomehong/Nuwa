import { clsx } from "clsx";
import { Inbox, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

export interface EmptyStateProps {
  title: string;
  description?: string;
  icon?: LucideIcon;
  action?: ReactNode;
  className?: string;
}

/** Neutral empty state with an explicit text channel (never colour-only). */
export function EmptyState({
  title,
  description,
  icon: Icon = Inbox,
  action,
  className,
}: EmptyStateProps) {
  return (
    <section
      role="status"
      className={clsx(
        "flex flex-col items-center justify-center gap-3 rounded-card border border-dashed border-border px-6 py-10 text-center",
        className,
      )}
    >
      <span
        aria-hidden="true"
        className="flex size-11 items-center justify-center rounded-full bg-raised text-text-muted"
      >
        <Icon className="size-5" strokeWidth={1.75} />
      </span>
      <div className="space-y-1">
        <h2 className="text-heading-sm font-semibold text-text">{title}</h2>
        {description ? (
          <p className="max-w-prose text-body-sm text-text-secondary">{description}</p>
        ) : null}
      </div>
      {action ? <div className="mt-1">{action}</div> : null}
    </section>
  );
}
