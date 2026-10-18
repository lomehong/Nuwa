import { AlertCircle, RefreshCw } from "lucide-react";
import type { ReactNode } from "react";

import { Button, Card, CardContent } from "../../components/ui";
import { describeError, describeErrorCode } from "../../lib/errors";

// Re-exported for the panels that already import these from the panel chrome.
export { describeError, describeErrorCode };

interface OverviewSectionProps {
  id: string;
  title: string;
  description?: string;
  /** Count badge or small controls shown opposite the title. */
  aside?: ReactNode;
  children: ReactNode;
  className?: string;
}

/** Consistent h2 landmark for every overview panel. */
export function OverviewSection({
  id,
  title,
  description,
  aside,
  children,
  className,
}: OverviewSectionProps) {
  return (
    <section aria-labelledby={id} className={className}>
      <div className="mb-3 flex flex-wrap items-end justify-between gap-2">
        <div className="min-w-0">
          <h2 id={id} className="text-heading font-semibold text-text">
            {title}
          </h2>
          {description ? (
            <p className="mt-0.5 text-body-sm text-text-secondary">{description}</p>
          ) : null}
        </div>
        {aside ? <div className="flex shrink-0 items-center gap-2">{aside}</div> : null}
      </div>
      {children}
    </section>
  );
}

interface PanelErrorProps {
  title: string;
  error: unknown;
  onRetry: () => void;
  /** Compact variant for cards with their own heading. */
  compact?: boolean;
}

/** Inline retryable error used by every independently-fetched panel. */
export function PanelError({ title, error, onRetry, compact = false }: PanelErrorProps) {
  const code = describeErrorCode(error);
  return (
    <Card role="alert" className="border-danger/40">
      <CardContent className={compact ? "p-4" : "flex flex-wrap items-start gap-3 p-4"}>
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <AlertCircle aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-danger" />
          <div className="min-w-0">
            <p className="text-body font-medium text-text">{title}</p>
            <p className="mt-0.5 text-body-sm text-text-secondary">
              {describeError(error)}
              {code ? (
                <span className="ml-1.5 font-mono text-caption text-text-muted">({code})</span>
              ) : null}
            </p>
          </div>
        </div>
        <div className="mt-3 shrink-0 sm:mt-0">
          <Button variant="secondary" size="sm" onClick={onRetry}>
            <RefreshCw aria-hidden="true" className="size-3.5" />
            重试
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
