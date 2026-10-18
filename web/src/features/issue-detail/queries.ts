/**
 * Issue detail data hook.
 *
 * docs/09 §12: the page polls every 15s, pauses while the tab is hidden and
 * refreshes immediately when the window regains focus. Background failures
 * keep the last successful payload on screen (`data` stays populated), so the
 * page can show a stale notice instead of discarding the audit trail.
 */

import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import type { IssueDetailResponse } from "./types";

export const ISSUE_DETAIL_POLL_MS = 15_000;

export function issueDetailQueryKey(fingerprint: string) {
  return ["issue", fingerprint] as const;
}

/** API-relative path for one issue; the fingerprint is one encoded segment. */
export function issueDetailApiPath(fingerprint: string): string {
  return `/issues/${encodeURIComponent(fingerprint)}`;
}

export function useIssueDetailQuery(fingerprint: string) {
  return useQuery({
    queryKey: issueDetailQueryKey(fingerprint),
    queryFn: ({ signal }) =>
      apiFetch<IssueDetailResponse>(issueDetailApiPath(fingerprint), { signal }),
    enabled: fingerprint.length > 0,
    staleTime: ISSUE_DETAIL_POLL_MS,
    refetchInterval: ISSUE_DETAIL_POLL_MS,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}
