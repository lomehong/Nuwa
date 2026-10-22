import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { backoffInterval } from "../../lib/polling";
import type { BenchmarkDetailResponse, BenchmarkRunsResponse } from "./types";

export const BENCHMARKS_POLL_MS = 15_000;

export const benchmarksQueryKey = ["benchmarks"] as const;

export function useBenchmarksQuery() {
  return useQuery({
    queryKey: benchmarksQueryKey,
    queryFn: ({ signal }) =>
      apiFetch<BenchmarkRunsResponse>("/benchmarks?limit=50", { signal }),
    staleTime: BENCHMARKS_POLL_MS,
    refetchInterval: backoffInterval(BENCHMARKS_POLL_MS),
    refetchIntervalInBackground: false,
    placeholderData: keepPreviousData,
  });
}

export const benchmarkQueryKey = (id: string) => ["benchmark", id] as const;

export function useBenchmarkQuery(id: string) {
  return useQuery({
    queryKey: benchmarkQueryKey(id),
    queryFn: ({ signal }) =>
      apiFetch<BenchmarkDetailResponse>(`/benchmarks/${encodeURIComponent(id)}`, { signal }),
    staleTime: 8_000,
    refetchInterval: (query) =>
      query.state.data?.run.status === "running" ? 5_000 : false,
    enabled: id.length > 0,
  });
}
