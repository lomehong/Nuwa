/**
 * Issue list data hook.
 *
 * docs/09 §12: the list polls every 15s, pauses while the tab is hidden and
 * refreshes immediately when the window regains focus; consecutive failures
 * back off exponentially up to 60s. The app-wide default is
 * `refetchOnWindowFocus: false`, so the page opts in explicitly. The previous
 * page/filter result stays on screen while the next one loads (`keepPreviousData`)
 * instead of collapsing to a skeleton on every keystroke or page click.
 */

import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { backoffInterval } from "../../lib/polling";
import { issueListPath, issueListQueryKey } from "./filters";
import type { IssueListFilters, IssueListResponse } from "./types";

export const ISSUES_POLL_MS = 15_000;

export function useIssuesQuery(filters: IssueListFilters) {
  return useQuery({
    queryKey: issueListQueryKey(filters),
    queryFn: ({ signal }) => apiFetch<IssueListResponse>(issueListPath(filters), { signal }),
    staleTime: ISSUES_POLL_MS,
    refetchInterval: backoffInterval(ISSUES_POLL_MS),
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
    placeholderData: keepPreviousData,
  });
}
