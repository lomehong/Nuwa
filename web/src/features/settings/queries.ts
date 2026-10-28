/**
 * Settings page data hooks (docs/09 §9.2 settings/system).
 *
 * Poll saved revisions and process activation. Form drafts are maintained
 * separately and take precedence over refreshed saved values.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { backoffInterval } from "../../lib/polling";
import { invalidateSettingsViews } from "../../lib/queryInvalidation";
import type {
  SaveSettingsResponse,
  SettingsResponse,
  SettingsValues,
  TestSettingsRequest,
  TestSettingsResponse,
} from "./types";

export const settingsQueryKey = ["settings"] as const;
export const SETTINGS_POLL_MS = 10_000;

export function useSettingsQuery() {
  return useQuery({
    queryKey: settingsQueryKey,
    queryFn: ({ signal }) => apiFetch<SettingsResponse>("/settings", { signal }),
    staleTime: 0,
    refetchInterval: backoffInterval(SETTINGS_POLL_MS),
    refetchIntervalInBackground: false,
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
      void queryClient.invalidateQueries({ queryKey: ["runners"] });
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
