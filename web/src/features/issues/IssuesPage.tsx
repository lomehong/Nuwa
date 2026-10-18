import { useCallback, useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";

import { useToast } from "../../components/ui";
import { buildIssueSearchParams, hasActiveFilters, parseIssueFilters } from "./filters";
import { shortFingerprint } from "./format";
import { IssueCards } from "./IssueCards";
import { IssueFilters } from "./IssueFilters";
import {
  IssueListEmpty,
  IssueListError,
  IssueListSkeleton,
  IssueListStaleNotice,
} from "./IssueListStates";
import { IssuePagination } from "./IssuePagination";
import { IssueTable } from "./IssueTable";
import { useIssuesQuery } from "./queries";
import type { IssueListItem, IssueListFilters, IssueSort } from "./types";

/**
 * Issue list (docs/09 §10.4).
 *
 * The URL is the source of truth: every filter, sort and page change rewrites
 * `?state=&severity=&repo=&language=&q=&stuck=&attention=&sort=&order=&page=&page_size=`
 * and the query key follows, so reload/share/back all reproduce the same view.
 * Filter changes reset `page` to 1; only the pagination controls change it.
 * Rows copy the fingerprint, open the detail page, or offer the state-aware
 * reopen/requeue action.
 */
export function IssuesPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const filters = useMemo(() => parseIssueFilters(searchParams), [searchParams]);
  const query = useIssuesQuery(filters);
  const { toast } = useToast();
  const [filterResetNonce, setFilterResetNonce] = useState(0);

  /** Applies filter/sort patches and resets to page 1. */
  const applyFilters = useCallback(
    (patch: Partial<IssueListFilters>) => {
      setSearchParams(buildIssueSearchParams({ ...filters, ...patch, page: 1 }));
    },
    [filters, setSearchParams],
  );

  const clearFilters = useCallback(() => {
    // Drop pending debounce drafts first: a term typed moments before the
    // click would otherwise commit after the URL had already been cleared.
    setFilterResetNonce((nonce) => nonce + 1);
    setSearchParams(
      buildIssueSearchParams({
        ...filters,
        states: [],
        severity: "",
        repo: "",
        language: "",
        q: "",
        stuck: false,
        attention: false,
        page: 1,
      }),
    );
  }, [filters, setSearchParams]);

  const goToPage = useCallback(
    (page: number) => {
      setSearchParams(buildIssueSearchParams({ ...filters, page: Math.max(1, page) }));
    },
    [filters, setSearchParams],
  );

  /** Header click: same field toggles direction, a new field starts at desc. */
  const handleSort = useCallback(
    (sort: IssueSort) => {
      if (filters.sort === sort) {
        applyFilters({ order: filters.order === "desc" ? "asc" : "desc" });
      } else {
        applyFilters({ sort, order: "desc" });
      }
    },
    [applyFilters, filters.order, filters.sort],
  );

  const copyFingerprint = useCallback(
    async (issue: IssueListItem) => {
      if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
        toast({
          title: "复制失败",
          description: "当前浏览器不支持剪贴板访问，请手动选择指纹复制。",
          variant: "danger",
        });
        return;
      }
      try {
        await navigator.clipboard.writeText(issue.fingerprint);
        toast({
          title: "已复制指纹",
          description: shortFingerprint(issue.fingerprint),
          variant: "success",
          duration: 2500,
        });
      } catch {
        toast({
          title: "复制失败",
          description: "无法访问剪贴板，请手动选择指纹复制。",
          variant: "danger",
        });
      }
    },
    [toast],
  );

  const repos = useMemo(() => {
    const values = new Set((query.data?.data ?? []).map((issue) => issue.repo).filter(Boolean));
    return Array.from(values).sort((left, right) => left.localeCompare(right));
  }, [query.data]);

  const data = query.data;
  const isInitialLoading = query.isPending && !data;
  const emptyKind =
    data && data.data.length === 0
      ? filters.page > 1 && data.meta.total > 0
        ? "page"
        : hasActiveFilters(filters)
          ? "filtered"
          : "no-data"
      : null;

  return (
    <div className="space-y-4">
      <header>
        <h2 className="text-title font-semibold text-text">问题单</h2>
        <p className="mt-1 text-body-sm text-text-secondary">
          按状态、严重度、仓库与语言筛选，查看每个指纹的修复进度。
        </p>
        <p className="mt-1 text-caption text-text-muted">
          每 15 秒自动刷新，标签页隐藏时暂停；窗口重新聚焦时立即更新。
        </p>
      </header>

      <IssueFilters
        filters={filters}
        repos={repos}
        onApply={applyFilters}
        onClear={clearFilters}
        resetNonce={filterResetNonce}
      />

      {query.isError && data ? (
        <IssueListStaleNotice error={query.error} onRetry={() => void query.refetch()} />
      ) : null}

      {isInitialLoading ? <IssueListSkeleton /> : null}

      {query.isError && !data ? (
        <IssueListError error={query.error} onRetry={() => void query.refetch()} />
      ) : null}

      {data && emptyKind && !query.isError ? (
        <IssueListEmpty
          kind={emptyKind}
          total={data.meta.total}
          onAction={emptyKind === "page" ? () => goToPage(1) : clearFilters}
        />
      ) : null}

      {data && data.data.length > 0 ? (
        <>
          <div className="space-y-3" aria-busy={query.isFetching || undefined}>
            <div className="hidden md:block">
              <IssueTable
                issues={data.data}
                sort={filters.sort}
                order={filters.order}
                onSort={handleSort}
                onCopyFingerprint={(issue) => void copyFingerprint(issue)}
              />
            </div>
            <div className="md:hidden">
              <IssueCards
                issues={data.data}
                onCopyFingerprint={(issue) => void copyFingerprint(issue)}
              />
            </div>
          </div>
          <IssuePagination
            meta={data.meta}
            page={filters.page}
            fetching={query.isFetching}
            onPageChange={goToPage}
          />
        </>
      ) : null}
    </div>
  );
}
