/**
 * Activity feed data hook (`GET /events`). Polling mirrors the operations
 * pages: 30s while visible, paused in a hidden tab, immediate refresh on focus.
 *
 * The query key is the filter tuple only — never `Date.now()` — so polls reuse
 * one cache entry; `from_ms` is computed inside the queryFn.
 */

import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { eventQueryString, type EventFilters } from "./filters";
import type { EventsResponse } from "./types";

export const EVENTS_POLL_MS = 30_000;

export function eventsQueryKey(filters: EventFilters) {
  return [
    "events",
    "filtered",
    filters.kinds.join(","),
    filters.repo,
    filters.fingerprint,
    filters.range,
    filters.aggregate,
    filters.page,
  ] as const;
}

export function useEventsQuery(filters: EventFilters) {
  return useQuery({
    queryKey: eventsQueryKey(filters),
    queryFn: ({ signal }) =>
      apiFetch<EventsResponse>(`/events?${eventQueryString(filters, Date.now())}`, {
        signal,
      }),
    staleTime: EVENTS_POLL_MS,
    refetchInterval: EVENTS_POLL_MS,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}
