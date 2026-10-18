/**
 * Runner fleet data hook. The roster changes only through heartbeats, so a 10s
 * poll keeps status fresh without hammering the store; like the other
 * operations pages it pauses while the tab is hidden and refreshes on focus.
 */

import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import type { RunnerListResponse } from "./types";

export const RUNNERS_POLL_MS = 10_000;

export function useRunnersQuery() {
  return useQuery({
    queryKey: ["runners"],
    queryFn: ({ signal }) => apiFetch<RunnerListResponse>("/runners", { signal }),
    staleTime: RUNNERS_POLL_MS,
    refetchInterval: RUNNERS_POLL_MS,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}
