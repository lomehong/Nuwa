/**
 * Overview page data hooks.
 *
 * Polling semantics are explicit here so the page and the shell's health strip
 * do not depend on app-wide defaults (docs/09 §10.9):
 *   - intervals pause while the tab is hidden (`refetchIntervalInBackground`
 *     defaults to false, but the page states it for clarity);
 *   - regaining focus refreshes immediately, overriding the app-wide
 *     `refetchOnWindowFocus: false` (a page that shows operations status must
 *     not display minutes-old data right after the operator returns).
 */

import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import {
  DEFAULT_TRENDS_RANGE,
  overviewQueryKey,
  recentEventsQueryKey,
  trendsQueryKey,
  type EventsResponse,
  type OverviewResponse,
  type TrendsRange,
  type TrendsResponse,
} from "./types";

export const OVERVIEW_POLL_MS = 10_000;
export const TRENDS_POLL_MS = 60_000;
export const EVENTS_POLL_MS = 30_000;
export const RECENT_EVENT_COUNT = 8;

function scopedPath(path: string, repo?: string | null): string {
  return repo ? `${path}?repo=${encodeURIComponent(repo)}` : path;
}

/** `/overview` for the page and the AppShell health strip (shared key). */
export function useOverviewQuery(repo?: string | null) {
  return useQuery({
    queryKey: overviewQueryKey(repo),
    queryFn: ({ signal }) => apiFetch<OverviewResponse>(scopedPath("/overview", repo), { signal }),
    staleTime: OVERVIEW_POLL_MS,
    refetchInterval: OVERVIEW_POLL_MS,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}

/** 14-day (by default) UTC daily buckets and range totals. */
export function useTrendsQuery(range: TrendsRange = DEFAULT_TRENDS_RANGE, repo?: string | null) {
  return useQuery({
    queryKey: trendsQueryKey(range, repo),
    queryFn: ({ signal }) => {
      const params = new URLSearchParams({ range });
      if (repo) {
        params.set("repo", repo);
      }
      return apiFetch<TrendsResponse>(`/metrics/trends?${params.toString()}`, { signal });
    },
    staleTime: TRENDS_POLL_MS,
    refetchInterval: TRENDS_POLL_MS,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}

/** Newest lifecycle events for the overview feed. */
export function useRecentEventsQuery(pageSize: number = RECENT_EVENT_COUNT) {
  return useQuery({
    queryKey: recentEventsQueryKey(pageSize),
    queryFn: ({ signal }) =>
      apiFetch<EventsResponse>(`/events?page_size=${pageSize}`, { signal }),
    staleTime: EVENTS_POLL_MS,
    refetchInterval: EVENTS_POLL_MS,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}
