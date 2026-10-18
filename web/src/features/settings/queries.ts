/**
 * Settings page data hooks (docs/09 §9.2 settings/system).
 *
 * Settings are the source of truth for the form, so the read is never cached
 * stale (`staleTime: 0`) and refreshes whenever the operator returns to the
 * tab. There is deliberately no interval: a background refetch would race the
 * dirty draft and replace values under the operator's cursor. `/system` is a
 * cheap read-only snapshot, refreshed at most every 30s.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { invalidateSettingsViews } from "../../lib/queryInvalidation";
import type {
  RestartServicesRequest,
  SaveSettingsResponse,
  SettingsResponse,
  SettingsValues,
  SystemResponse,
  TestSettingsRequest,
  TestSettingsResponse,
} from "./types";

export const SYSTEM_STALE_MS = 30_000;

export const settingsQueryKey = ["settings"] as const;
export const systemQueryKey = ["system"] as const;

export function useSettingsQuery() {
  return useQuery({
    queryKey: settingsQueryKey,
    queryFn: ({ signal }) => apiFetch<SettingsResponse>("/settings", { signal }),
    staleTime: 0,
    refetchOnWindowFocus: true,
  });
}

export function useSystemQuery() {
  return useQuery({
    queryKey: systemQueryKey,
    queryFn: ({ signal }) => apiFetch<SystemResponse>("/system", { signal }),
    staleTime: SYSTEM_STALE_MS,
    refetchOnWindowFocus: true,
  });
}

/** `PUT /settings` writes only the keys in `values` (docs/09 §9.2). */
export function useSaveSettingsMutation() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (values: SettingsValues) =>
      apiFetch<SaveSettingsResponse>("/settings", {
        method: "PUT",
        body: JSON.stringify({ values }),
      }),
    onSuccess: () => {
      invalidateSettingsViews(queryClient);
    },
  });
}

/** `POST /settings/test` probes GitHub or the LLM; nothing is persisted. */
export function useTestSettingsMutation() {
  return useMutation({
    mutationFn: (request: TestSettingsRequest) =>
      apiFetch<TestSettingsResponse>("/settings/test", {
        method: "POST",
        body: JSON.stringify(request),
      }),
  });
}

/**
 * `POST /settings/restart` schedules a systemd restart (202). A 409 carries
 * the guarded `active_runs` list in the `ApiError.body`; Task 7 renders it.
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
