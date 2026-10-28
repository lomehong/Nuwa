/**
 * System page data hooks (docs/09 §9.2 system). A cheap read-only snapshot;
 * the restart mutation still lives on the settings API but is only reachable
 * from this page.
 */

import { useMutation, useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { backoffInterval } from "../../lib/polling";
import type { RestartServicesRequest, SystemResponse } from "./types";

export const SYSTEM_STALE_MS = 30_000;

export const systemQueryKey = ["system"] as const;

export function useSystemQuery() {
  return useQuery({
    queryKey: systemQueryKey,
    queryFn: ({ signal }) => apiFetch<SystemResponse>("/system", { signal }),
    staleTime: SYSTEM_STALE_MS,
    refetchInterval: backoffInterval(SYSTEM_STALE_MS),
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}

/**
 * `POST /settings/restart` schedules a systemd restart (202). A 409 carries
 * the guarded `active_runs` list in the `ApiError.body`; RestartDialog renders it.
 */
export function useRestartServicesMutation() {
  return useMutation({
    mutationFn: (request: RestartServicesRequest = {}) =>
      apiFetch<void>("/settings/restart", {
        method: "POST",
        body: JSON.stringify({ force: request.force ?? false }),
      }),
  });
}
