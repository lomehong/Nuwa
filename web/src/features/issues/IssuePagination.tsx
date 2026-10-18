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
}

/**
 * Server-side pagination footer (docs/09 §10.4): total, current page and
 * prev/next. Page changes write the URL, which drives the request. Buttons are
 * disabled at the bounds and while a page is in flight.
 */
export function IssuePagination({ meta, page, fetching, onPageChange }: IssuePaginationProps) {
  const pageSize = meta.page_size > 0 ? meta.page_size : 1;
  const pageCount = Math.max(1, Math.ceil(meta.total / pageSize));

  return (
    <nav
      aria-label="问题单分页"
      className="flex flex-wrap items-center justify-between gap-3 rounded-card border border-border bg-surface px-4 py-3"
    >
      <p className="text-body-sm text-text-secondary">
        共 <span className="font-mono tabular-nums text-text">{formatCount(meta.total)}</span> 条
        <span aria-hidden="true"> · </span>
        {/* The URL page is authoritative: `meta.page` trails behind while
            `keepPreviousData` paints the previous page during a transition. */}
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
