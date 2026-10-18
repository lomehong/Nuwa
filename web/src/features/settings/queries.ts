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
  CreateRepoRequest,
  RepoListResponse,
  RepoResponse,
  RestartServicesRequest,
  SaveSettingsResponse,
  SettingsResponse,
  SettingsValues,
  SystemResponse,
  TestSettingsRequest,
  TestSettingsResponse,
  UpdateRepoRequest,
} from "./types";

export const SYSTEM_STALE_MS = 30_000;
export const REPOS_STALE_MS = 30_000;

export const settingsQueryKey = ["settings"] as const;
export const systemQueryKey = ["system"] as const;
export const reposQueryKey = ["repos"] as const;

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

/**
 * The path segment for one registry entry. Axum's `{repo}` matches a single
 * segment, while identities contain `/`, so percent-encoding is mandatory.
 */
export function repoApiPath(repo: string): string {
  return `/repos/${encodeURIComponent(repo)}`;
}

/** `GET /repos` — the registry is small and changes rarely (docs/09 §8.7). */
export function useReposQuery() {
  return useQuery({
    queryKey: reposQueryKey,
    queryFn: ({ signal }) => apiFetch<RepoListResponse>("/repos", { signal }),
    staleTime: REPOS_STALE_MS,
    refetchOnWindowFocus: true,
  });
}

export function useCreateRepoMutation() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (request: CreateRepoRequest) =>
      apiFetch<RepoResponse>("/repos", {
        method: "POST",
        body: JSON.stringify(request),
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: reposQueryKey });
    },
  });
}

export function useUpdateRepoMutation() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ repo, ...fields }: UpdateRepoRequest & { repo: string }) =>
      apiFetch<RepoResponse>(repoApiPath(repo), {
        method: "PUT",
        body: JSON.stringify(fields),
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: reposQueryKey });
    },
  });
}

export function useDeleteRepoMutation() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (repo: string) =>
      apiFetch<void>(repoApiPath(repo), { method: "DELETE" }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: reposQueryKey });
    },
  });
}
