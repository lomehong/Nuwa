import type { ReactNode } from "react";

interface IssueSectionProps {
  id: string;
  title: string;
  description?: string;
  /** Count badge or controls shown opposite the title. */
  aside?: ReactNode;
  children: ReactNode;
  className?: string;
}

/**
 * Consistent h3 landmark for every detail panel. The shell owns the page h1
 * ("问题单详情") and the issue header owns the h2 (the title), so sections
 * stay at h3 even when they contain cards with their own small headings.
 */
export function IssueSection({
  id,
  title,
  description,
  aside,
  children,
  className,
}: IssueSectionProps) {
  return (
    <section aria-labelledby={id} className={className}>
      <div className="mb-3 flex flex-wrap items-end justify-between gap-2">
        <div className="min-w-0">
          <h3 id={id} className="text-heading-sm font-semibold text-text">
            {title}
          </h3>
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
