import { ChevronLeft, ChevronRight } from "lucide-react";

import { Button } from "../../components/ui";
import { formatCount } from "./format";
import type { IssueListMeta } from "./types";

interface IssuePaginationProps {
  meta: IssueListMeta;
  /** Requested page (already mirrored in the URL). */
  page: number;
  fetching: boolean;
  onPageChange: (page: number) => void;
  /** Accessible name; other list pages pass their own label. */
  label?: string;
}

/**
 * Server-side pagination footer (docs/09 §10.4): total, current page and
 * prev/next. Page changes write the URL, which drives the request. Buttons are
 * disabled at the bounds and while a page is in flight.
 */
export function IssuePagination({
  meta,
  page,
  fetching,
  onPageChange,
  label = "问题单分页",
}: IssuePaginationProps) {
  const pageSize = meta.page_size > 0 ? meta.page_size : 1;
  const pageCount = Math.max(1, Math.ceil(meta.total / pageSize));

  return (
    <nav
      aria-label={label}
      className="flex flex-wrap items-center justify-between gap-3 rounded-card border border-border bg-surface px-4 py-3"
    >
      <p className="text-body-sm text-text-secondary">
        共 <span className="font-mono tabular-nums text-text">{formatCount(meta.total)}</span> 条
        <span aria-hidden="true"> · </span>
        {/* The URL page is authoritative: `meta.page` trails behind while
            previous-page placeholder data paints during a filter/page
            transition (the events feed keeps its rows via keepPreviousData). */}
        第 <span className="font-mono tabular-nums text-text">{page}</span> /{" "}
        <span className="font-mono tabular-nums text-text">{pageCount}</span> 页
      </p>
      <div className="flex items-center gap-2">
        <Button
          variant="secondary"
          size="sm"
          onClick={() => onPageChange(page - 1)}
          disabled={page <= 1 || fetching}
        >
          <ChevronLeft aria-hidden="true" className="size-3.5" />
          上一页
        </Button>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => onPageChange(page + 1)}
          disabled={page >= pageCount || fetching}
        >
          下一页
          <ChevronRight aria-hidden="true" className="size-3.5" />
        </Button>
      </div>
    </nav>
  );
}
