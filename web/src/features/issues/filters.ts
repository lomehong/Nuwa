/**
 * URL ⇄ filter-state conversion for the issue list.
 *
 * The URL is the single source of truth: filters, sort and page survive a
 * reload and can be shared. Parsing is deliberately forgiving — invalid or
 * unknown values fall back to the server defaults instead of forwarding a
 * request that would be rejected with 422. Writing only emits non-default
 * values so links stay short and stable.
 */

import {
  ISSUE_STATE_VALUES,
  type IssueState,
  type Severity,
} from "../../components/ui";
import {
  DEFAULT_ISSUE_FILTERS,
  DEFAULT_PAGE_SIZE,
  ISSUE_ORDER_VALUES,
  ISSUE_SORT_VALUES,
  MAX_PAGE_SIZE,
  SEVERITY_FILTER_VALUES,
  type IssueListFilters,
  type IssueSort,
  type IssueSortOrder,
} from "./types";

const STATE_VALUES: readonly string[] = ISSUE_STATE_VALUES;
const SEVERITY_VALUES: readonly string[] = SEVERITY_FILTER_VALUES;

export function isIssueState(value: string): value is IssueState {
  return STATE_VALUES.includes(value);
}

export function isFilterSeverity(value: string): value is Severity {
  return SEVERITY_VALUES.includes(value);
}

export function isIssueSort(value: string): value is IssueSort {
  return (ISSUE_SORT_VALUES as readonly string[]).includes(value);
}

export function isIssueOrder(value: string): value is IssueSortOrder {
  return (ISSUE_ORDER_VALUES as readonly string[]).includes(value);
}

/** `1|true` enables a flag; anything else (including absent) is false. */
function parseFlag(value: string | null): boolean {
  return value === "1" || value === "true";
}

function parsePositiveInt(value: string | null, fallback: number): number {
  if (!value) {
    return fallback;
  }
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) {
    return fallback;
  }
  const parsed = Number(trimmed);
  return Number.isSafeInteger(parsed) && parsed >= 1 ? parsed : fallback;
}

/** Deduplicate and order by the lifecycle enum so URLs and keys are stable. */
export function canonicalStates(states: readonly string[]): IssueState[] {
  const requested = new Set(states.filter(isIssueState));
  return ISSUE_STATE_VALUES.filter((state) => requested.has(state));
}

export function parseIssueFilters(params: URLSearchParams): IssueListFilters {
  const severityRaw = params.get("severity") ?? "";
  const sortRaw = params.get("sort") ?? "";
  const orderRaw = params.get("order") ?? "";
  const pageSizeRaw = parsePositiveInt(params.get("page_size"), DEFAULT_PAGE_SIZE);

  return {
    states: canonicalStates((params.get("state") ?? "").split(",").map((part) => part.trim())),
    severity: isFilterSeverity(severityRaw) ? severityRaw : "",
    repo: (params.get("repo") ?? "").trim(),
    language: (params.get("language") ?? "").trim(),
    q: (params.get("q") ?? "").trim(),
    stuck: parseFlag(params.get("stuck")),
    attention: parseFlag(params.get("attention")),
    sort: isIssueSort(sortRaw) ? sortRaw : DEFAULT_ISSUE_FILTERS.sort,
    order: isIssueOrder(orderRaw) ? orderRaw : DEFAULT_ISSUE_FILTERS.order,
    page: parsePositiveInt(params.get("page"), 1),
    pageSize:
      pageSizeRaw <= MAX_PAGE_SIZE ? pageSizeRaw : DEFAULT_ISSUE_FILTERS.pageSize,
  };
}

export function buildIssueSearchParams(filters: IssueListFilters): URLSearchParams {
  const params = new URLSearchParams();
  const state = canonicalStates(filters.states);

  if (state.length > 0) {
    params.set("state", state.join(","));
  }
  if (isFilterSeverity(filters.severity)) {
    params.set("severity", filters.severity);
  }

  const repo = filters.repo.trim();
  const language = filters.language.trim();
  const q = filters.q.trim();
  if (repo) {
    params.set("repo", repo);
  }
  if (language) {
    params.set("language", language);
  }
  if (q) {
    params.set("q", q);
  }
  if (filters.stuck) {
    params.set("stuck", "1");
  }
  if (filters.attention) {
    params.set("attention", "1");
  }

  if (filters.sort !== DEFAULT_ISSUE_FILTERS.sort) {
    params.set("sort", filters.sort);
  }
  if (filters.order !== DEFAULT_ISSUE_FILTERS.order) {
    params.set("order", filters.order);
  }

  const page = Number.isSafeInteger(filters.page) && filters.page >= 1 ? filters.page : 1;
  if (page !== 1) {
    params.set("page", String(page));
  }

  const pageSize =
    Number.isSafeInteger(filters.pageSize) &&
    filters.pageSize >= 1 &&
    filters.pageSize <= MAX_PAGE_SIZE
      ? filters.pageSize
      : DEFAULT_PAGE_SIZE;
  if (pageSize !== DEFAULT_PAGE_SIZE) {
    params.set("page_size", String(pageSize));
  }

  return params;
}

/** API-relative path for `GET /issues`, e.g. `/issues?state=escalated&page=2`. */
export function issueListPath(filters: IssueListFilters): string {
  const query = buildIssueSearchParams(filters).toString();
  return query ? `/issues?${query}` : "/issues";
}

/** Stable TanStack Query key; the serialized query is the canonical identity. */
export function issueListQueryKey(filters: IssueListFilters) {
  return ["issues", "list", buildIssueSearchParams(filters).toString()] as const;
}

/** "Clear filters" only concerns filter values, not sort or page size. */
export function hasActiveFilters(filters: IssueListFilters): boolean {
  return (
    filters.states.length > 0 ||
    filters.severity !== "" ||
    filters.repo.trim() !== "" ||
    filters.language.trim() !== "" ||
    filters.q.trim() !== "" ||
    filters.stuck ||
    filters.attention
  );
}
