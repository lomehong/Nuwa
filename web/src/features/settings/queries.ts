/**
 * Settings page data hooks (docs/09 §9.2 settings/system).
 *
 * Settings are the source of truth for the form, so the read is never cached
 * stale (`staleTime: 0`) and refreshes whenever the operator returns to the
 * tab. There is deliberately no interval: a background refetch would race the
 * dirty draft and replace values under the operator's cursor.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { invalidateSettingsViews } from "../../lib/queryInvalidation";
import type {
  SaveSettingsResponse,
  SettingsResponse,
  SettingsValues,
  TestSettingsRequest,
  TestSettingsResponse,
} from "./types";

export const settingsQueryKey = ["settings"] as const;

export function useSettingsQuery() {
  return useQuery({
    queryKey: settingsQueryKey,
    queryFn: ({ signal }) => apiFetch<SettingsResponse>("/settings", { signal }),
    staleTime: 0,
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
