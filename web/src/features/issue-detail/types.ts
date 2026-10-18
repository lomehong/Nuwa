/**
 * Response types for `GET /api/v1/issues/{fp}` (docs/09 §9.2, §10.5).
 *
 * `issue` is the same row shape as the list endpoint (plus the per-owner
 * `stuck` flag). `runs` is capped server-side at the newest 10; `timeline`
 * at the newest 200. The optional fields below are forward-compatible
 * additions: the current server sends `similar.items` with just
 * fingerprint/state/last_seen_ms and timeline rows without `count` or
 * `fingerprint`, but the UI already renders them when present (aggregated
 * history, system events) instead of dropping the information.
 */

import type { IssueListItem } from "../issues/types";

/** One lifecycle event on the issue timeline (newest first). */
export interface TimelineItem {
  kind: string;
  detail: string;
  ts_ms: number;
  actor: string | null;
  repo: string;
  /** Aggregated historical reports ("共 N 次"); absent on raw events. */
  count?: number;
  /**
   * Event owner when the server includes it. System fingerprints
   * (`__config__`, `__system__`, `__runner__:<id>`) never link to an issue.
   */
  fingerprint?: string;
}

/** One repair run (the newest 10 are returned). */
export interface RunDto {
  id: string;
  started_at_ms: number;
  finished_at_ms: number | null;
  phase: string;
  outcome: string | null;
  ci_verdict: string | null;
  turns: number;
  tokens_uncached: number;
  tokens_cached: number;
  provider: string | null;
  model: string | null;
  root_cause: string | null;
  change_stat: string | null;
  branch: string | null;
  commit_sha: string | null;
  pr_url: string | null;
  pr_opened_at_ms: number | null;
  verification_ok: boolean | null;
  verification_detail: string | null;
  error: string | null;
}

/** Same-title neighbour. Extra fields are optional until the server adds them. */
export interface SimilarItem {
  fingerprint: string;
  state: string;
  last_seen_ms: number;
  title?: string;
  repo?: string;
  severity?: string;
  occurrences?: number;
}

export interface SimilarIssues {
  count: number;
  items: SimilarItem[];
}

export interface IssueDetailResponse {
  issue: IssueListItem;
  stack_sample: string;
  timeline: TimelineItem[];
  runs: RunDto[];
  similar: SimilarIssues;
}
