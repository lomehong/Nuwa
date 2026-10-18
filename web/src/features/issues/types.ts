/**
 * Types and vocabulary for the issue list (docs/09 §10.4, §9.2).
 *
 * The list endpoint is read-only: filters, sorting and pagination are mirrored
 * in the URL (`useSearchParams`) so a reload or a shared link restores exactly
 * the same server query. Row actions reuse the issue mutation hooks.
 */

import type { IssueState, Severity } from "../../components/ui";

/** One row of `GET /api/v1/issues`. */
export interface IssueListItem {
  fingerprint: string;
  title: string;
  language: string;
  repo: string;
  severity: Severity;
  state: IssueState;
  occurrences: number;
  pr_url: string | null;
  first_seen_ms: number;
  last_seen_ms: number;
  state_changed_at_ms: number;
  owner_runner_id: string | null;
  /** Newest run verdict: success|failure|timeout|unavailable (null = no run). */
  ci_verdict: string | null;
  /** Server-side per-owner stuck detection (heartbeat expired). */
  stuck: boolean;
}

export interface IssueListMeta {
  page: number;
  page_size: number;
  total: number;
}

export interface IssueListResponse {
  data: IssueListItem[];
  meta: IssueListMeta;
}

export const ISSUE_SORT_VALUES = [
  "last_seen",
  "first_seen",
  "occurrences",
  "severity",
  "state",
] as const;

export type IssueSort = (typeof ISSUE_SORT_VALUES)[number];

export const ISSUE_SORT_LABELS: Record<IssueSort, string> = {
  last_seen: "最近出现",
  first_seen: "首次出现",
  occurrences: "出现次数",
  severity: "严重度",
  state: "状态",
};

export const ISSUE_ORDER_VALUES = ["asc", "desc"] as const;

export type IssueSortOrder = (typeof ISSUE_ORDER_VALUES)[number];

export const ISSUE_ORDER_LABELS: Record<IssueSortOrder, string> = {
  asc: "升序",
  desc: "降序",
};

/** Server default and hard cap for `page_size` (crates/nuwa/src/api/common.rs). */
export const DEFAULT_PAGE_SIZE = 50;
export const MAX_PAGE_SIZE = 200;
export const PAGE_SIZE_OPTIONS = [25, 50, 100, 200] as const;

/**
 * Server cap for the free-text search term (`MAX_Q_CHARS` in
 * crates/nuwa/src/api/issues.rs). Longer values are rejected with a 422, so
 * the input and the URL serialization clamp to the same limit.
 */
export const MAX_SEARCH_QUERY_CHARS = 128;

/** Severity values accepted by the list endpoint (no `unknown` filter). */
export const SEVERITY_FILTER_VALUES = ["critical", "high", "medium", "low"] as const;

export interface IssueListFilters {
  states: IssueState[];
  severity: Severity | "";
  repo: string;
  language: string;
  q: string;
  stuck: boolean;
  attention: boolean;
  sort: IssueSort;
  order: IssueSortOrder;
  /** 1-based page number. */
  page: number;
  pageSize: number;
}

export const DEFAULT_ISSUE_FILTERS: IssueListFilters = {
  states: [],
  severity: "",
  repo: "",
  language: "",
  q: "",
  stuck: false,
  attention: false,
  sort: "last_seen",
  order: "desc",
  page: 1,
  pageSize: DEFAULT_PAGE_SIZE,
};
