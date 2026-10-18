/**
 * Response types for the overview page's three read endpoints (docs/09 §9.2):
 * `GET /overview?repo=`, `GET /metrics/trends?range=14d&repo=` and
 * `GET /events?page_size=8`.
 *
 * The shell's HealthStrip shares `OverviewResponse` and the query key.
 */

import type { IssueState, Severity } from "../../components/ui";

export interface OverviewAttentionItem {
  fingerprint: string;
  title: string;
  state: IssueState;
  severity: Severity;
  repo: string;
  last_seen_ms: number;
  stuck: boolean;
  owner_runner_id: string | null;
  reason_code: string;
  reason_text: string;
  pr_url: string | null;
}

export interface OverviewActiveRun {
  id: string;
  fingerprint: string;
  title: string;
  repo: string;
  runner_id: string;
  phase: string;
  started_at_ms: number;
  turns: number;
  tokens: number;
}

export interface OverviewQueueItem {
  fingerprint: string;
  title: string;
  severity: Severity;
  occurrences: number;
}

export interface OverviewQueue {
  ready_count: number;
  next: OverviewQueueItem[];
}

export interface OverviewHealth {
  runners_total: number;
  runners_online: number;
  working: number;
  idle: number;
  standing_down: number;
  error: number;
  reports_today: number;
  prs_today: number;
}

export interface OverviewCiCounts {
  green: number;
  red: number;
  timeout: number;
  pending: number;
}

export interface OverviewResponse {
  counts: Record<IssueState, number>;
  attention: OverviewAttentionItem[];
  active_runs: OverviewActiveRun[];
  queue: OverviewQueue;
  health: OverviewHealth;
  ci_24h: OverviewCiCounts;
}

/** Shared TanStack Query key: `repo` undefined means the whole instance. */
export function overviewQueryKey(repo?: string | null) {
  return ["overview", repo ?? null] as const;
}

export type TrendsRange = "7d" | "14d" | "30d" | "90d";

export const DEFAULT_TRENDS_RANGE: TrendsRange = "14d";

/** One UTC day bucket; missing days arrive zero-filled from the API. */
export interface TrendBucket {
  day: string;
  reports: number;
  new_issues: number;
  prs_opened: number;
  merged: number;
  rejected: number;
  tokens: number;
  ci_green: number;
  ci_red: number;
}

export interface TrendTotals {
  reports: number;
  new_issues: number;
  prs_opened: number;
  merged: number;
  rejected: number;
  tokens: number;
  median_time_to_pr_secs: number | null;
}

export interface TrendsResponse {
  range: string;
  buckets: TrendBucket[];
  totals: TrendTotals;
}

export function trendsQueryKey(range: TrendsRange = DEFAULT_TRENDS_RANGE, repo?: string | null) {
  return ["trends", range, repo ?? null] as const;
}

export function recentEventsQueryKey(pageSize: number) {
  return ["events", "recent", pageSize] as const;
}
