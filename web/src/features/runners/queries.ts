/**
 * Runner fleet data hook. The roster changes only through heartbeats, so a 10s
 * poll keeps status fresh without hammering the store; like the other
 * operations pages it pauses while the tab is hidden, refreshes on focus and
 * backs off exponentially (up to 60s) while fetches keep failing.
 */

import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { backoffInterval } from "../../lib/polling";
import type { RunnerListResponse } from "./types";

export const RUNNERS_POLL_MS = 10_000;

export function useRunnersQuery() {
  return useQuery({
    queryKey: ["runners"],
    queryFn: ({ signal }) => apiFetch<RunnerListResponse>("/runners", { signal }),
    staleTime: RUNNERS_POLL_MS,
    refetchInterval: backoffInterval(RUNNERS_POLL_MS),
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}
