/**
 * Response types for `GET /api/v1/overview?repo=` (docs/09 §9.2).
 *
 * The shell's HealthStrip consumes the `health`, `queue` and `active_runs`
 * slices; Task 4 fills in the full overview page against the same shape.
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
