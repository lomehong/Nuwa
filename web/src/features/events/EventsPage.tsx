import { Activity, FileQuestion, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef } from "react";
import { useSearchParams } from "react-router-dom";

import {
  Badge,
  Button,
  Card,
  CardContent,
  EmptyState,
  Skeleton,
  Table,
  TableBody,
  TableHead,
  TableHeaderCell,
} from "../../components/ui";
import { hasVisibleModal } from "../../lib/modal";
import { describeError, PanelError } from "../overview/PanelChrome";
import { IssuePagination } from "../issues/IssuePagination";
import {
  hasActiveEventFilters,
  EventFilters,
  isAuditView,
} from "./EventFilters";
import { EventCard, EventRow } from "./EventRow";
import { DEFAULT_EVENT_FILTERS, eventPagePath, parseEventFilters, type EventFilters as EventFilterState } from "./filters";
import { useEventsQuery } from "./queries";

const SKELETON_ROWS = 6;

function EventsSkeleton() {
  return (
    <div role="status" aria-busy="true">
      <span className="sr-only">正在加载活动</span>
      <Card>
        <CardContent className="space-y-3 p-4">
          {Array.from({ length: SKELETON_ROWS }, (_, index) => (
            <div key={index} className="flex items-center gap-3">
              <Skeleton className="h-4 w-16 shrink-0" />
              <Skeleton rounded="full" className="h-5 w-20 shrink-0" />
              <Skeleton className="h-4 flex-1" />
              <Skeleton className="hidden h-4 w-24 md:block" />
            </div>
          ))}
        </CardContent>
      </Card>
    </div>
  );
}

interface EventsEmptyProps {
  pageOutOfRange: boolean;
  total: number;
  filtered: boolean;
  onReset: () => void;
}

function EventsEmpty({ pageOutOfRange, total, filtered, onReset }: EventsEmptyProps) {
  if (pageOutOfRange) {
    return (
      <EmptyState
        icon={FileQuestion}
        title="该页没有事件"
        description={`共 ${total} 条事件，当前页超出范围。回到第一页继续查看。`}
        action={
          <Button variant="secondary" size="sm" onClick={onReset}>
            回到第一页
          </Button>
        }
      />
    );
  }
  return (
    <EmptyState
      icon={Activity}
      title="这个时间窗内没有事件"
      description="调整类型或时间范围，或等待新的上报/运行记录。"
      action={
        filtered ? (
          <Button variant="secondary" size="sm" onClick={onReset}>
            清除筛选
          </Button>
        ) : undefined
      }
    />
  );
}

/**
 * Activity and audit feed (docs/09 §10.7).
 *
 * The URL is the source of truth: kind chips, the audit preset, repo,
 * fingerprint, range, aggregation and page all round-trip through
 * `parseEventFilters`/`eventPagePath`. Filter changes reset the page to 1;
 * only the pagination controls move it. System fingerprints stay unlinked
 * because they have no issue detail page. A sync button arrives with M5.
 */
export function EventsPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const filters = useMemo(() => parseEventFilters(searchParams), [searchParams]);
  const query = useEventsQuery(filters);
  const auditView = isAuditView(filters);
  const searchInputRef = useRef<HTMLInputElement>(null);

  // docs/09 §12: `/` focuses the page's first filter field, same contract as
  // the issue list (skipped while a modal or another editable control owns
  // the keyboard; Escape only blurs).
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const active = document.activeElement;
      if (event.key === "Escape") {
        if (active === searchInputRef.current) {
          event.preventDefault();
          searchInputRef.current?.blur();
        }
        return;
      }
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) {
        return;
      }
      if (hasVisibleModal()) {
        return;
      }
      if (
        active instanceof HTMLElement &&
        (active.isContentEditable ||
          active.tagName === "INPUT" ||
          active.tagName === "TEXTAREA" ||
          active.tagName === "SELECT")
      ) {
        return;
      }
      event.preventDefault();
      searchInputRef.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const applyFilters = useCallback(
    (patch: Partial<EventFilterState>) => {
      // A patch without `page` is a filter change and returns to the first page.
      const page = patch.page ?? 1;
      const path = eventPagePath({ ...filters, ...patch, page });
      const queryIndex = path.indexOf("?");
      setSearchParams(
        new URLSearchParams(queryIndex === -1 ? "" : path.slice(queryIndex + 1)),
      );
    },
    [filters, setSearchParams],
  );

  const clearFilters = useCallback(() => {
    applyFilters({ ...DEFAULT_EVENT_FILTERS });
  }, [applyFilters]);

  const data = query.data;
  const isInitialLoading = query.isPending && !data;
  const pageOutOfRange = Boolean(
    data && data.data.length === 0 && filters.page > 1 && data.meta.total > 0,
  );
  const showEmpty = Boolean(data && data.data.length === 0 && !query.isError);

  return (
    <div className="space-y-4">
      <header>
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-title font-semibold text-text">活动</h2>
          {auditView ? <Badge variant="accent">审计视图</Badge> : null}
        </div>
        <p className="mt-1 text-body-sm text-text-secondary">
          生命周期事件与系统审计记录，按时间倒序；历史同类上报可按小时聚合。
        </p>
        <p className="mt-1 text-caption text-text-muted">
          每 30 秒自动刷新，标签页隐藏时暂停；窗口重新聚焦时立即更新。
        </p>
      </header>

      <EventFilters
        filters={filters}
        onChange={applyFilters}
        searchInputRef={searchInputRef}
      />

      {query.isError && data ? (
        <div className="flex flex-wrap items-start justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text">
          <p role="status" className="min-w-0 flex-1">
            活动更新失败，显示的是上次数据。
            <span className="ml-1 text-text-secondary">{describeError(query.error)}</span>
          </p>
          <Button variant="ghost" size="sm" onClick={() => void query.refetch()}>
            <RefreshCw aria-hidden="true" className="size-3.5" />
            重试
          </Button>
        </div>
      ) : null}

      {isInitialLoading ? <EventsSkeleton /> : null}

      {query.isError && !data ? (
        <PanelError
          title="无法加载活动"
          error={query.error}
          onRetry={() => void query.refetch()}
        />
      ) : null}

      {showEmpty && !pageOutOfRange ? (
        <EventsEmpty
          pageOutOfRange={false}
          total={data?.meta.total ?? 0}
          filtered={hasActiveEventFilters(filters)}
          onReset={clearFilters}
        />
      ) : null}

      {data && pageOutOfRange && !query.isError ? (
        <EventsEmpty
          pageOutOfRange
          total={data.meta.total}
          filtered={hasActiveEventFilters(filters)}
          onReset={() => applyFilters({ page: 1 })}
        />
      ) : null}

      {data && data.data.length > 0 ? (
        <>
          <div className="space-y-3" aria-busy={query.isFetching || undefined}>
            <div className="hidden md:block">
              <Table label="活动列表" caption="活动列表">
                <TableHead>
                  <tr>
                    <TableHeaderCell>时间</TableHeaderCell>
                    <TableHeaderCell>类型</TableHeaderCell>
                    <TableHeaderCell>问题单</TableHeaderCell>
                    <TableHeaderCell>详情</TableHeaderCell>
                    <TableHeaderCell>仓库</TableHeaderCell>
                    <TableHeaderCell>操作者</TableHeaderCell>
                  </tr>
                </TableHead>
                <TableBody>
                  {data.data.map((event, index) => (
                    <EventRow
                      key={`${event.fingerprint}-${event.kind}-${event.ts_ms}-${index}`}
                      event={event}
                    />
                  ))}
                </TableBody>
              </Table>
            </div>
            <div className="md:hidden">
              <ul aria-label="活动卡片列表" className="space-y-3">
                {data.data.map((event, index) => (
                  <EventCard
                    key={`${event.fingerprint}-${event.kind}-${event.ts_ms}-${index}`}
                    event={event}
                  />
                ))}
              </ul>
            </div>
          </div>
          <IssuePagination
            label="活动分页"
            meta={data.meta}
            page={filters.page}
            fetching={query.isFetching}
            onPageChange={(page) => applyFilters({ page: Math.max(1, page) })}
          />
        </>
      ) : null}
    </div>
  );
}
