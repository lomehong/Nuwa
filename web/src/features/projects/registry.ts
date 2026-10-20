/**
 * 仓库登记（registry）——控制台侧的**展示身份**。
 *
 * 登记只承载 name / url / description：仓库内容（failing_check、
 * forbidden_paths、notes）始终来自目标仓的 `nuwa.repo.json`，只读。
 * 登记与"项目"是同一个对象，所以这一层住在 projects 特性里：项目页
 * 与设置页共用同一份数据与同一套表单。
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { projectsQueryKey } from "./queries";

/**
 * A console-registered repository (`crates/nuwa/src/api/repos.rs`).
 *
 * Only the display identity lives here. Repository content (`failing_check`,
 * `forbidden_paths`, `notes`) stays in the target repo's `nuwa.repo.json` and
 * is never part of this API.
 */
export interface RepoDto {
  repo: string;
  name: string;
  url: string;
  description: string;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface RepoListResponse {
  data: RepoDto[];
}

export interface RepoResponse {
  data: RepoDto;
}

export interface CreateRepoRequest {
  repo: string;
  name: string;
  url: string;
  description: string;
}

/** The identity is path-owned and immutable, so it never travels in the body. */
export type UpdateRepoRequest = Omit<CreateRepoRequest, "repo">;

export const REPOS_STALE_MS = 30_000;

export const reposQueryKey = ["repos"] as const;

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

/** A registry write changes both the registry list and the project rollup. */
function invalidateRegistryViews(
  queryClient: ReturnType<typeof useQueryClient>,
): void {
  void queryClient.invalidateQueries({ queryKey: reposQueryKey });
  void queryClient.invalidateQueries({ queryKey: projectsQueryKey });
}

export function useCreateRepoMutation() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (request: CreateRepoRequest) =>
      apiFetch<RepoResponse>("/repos", {
        method: "POST",
        body: JSON.stringify(request),
      }),
    onSuccess: () => invalidateRegistryViews(queryClient),
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
    onSuccess: () => invalidateRegistryViews(queryClient),
  });
}

export function useDeleteRepoMutation() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (repo: string) =>
      apiFetch<void>(repoApiPath(repo), { method: "DELETE" }),
    onSuccess: () => invalidateRegistryViews(queryClient),
  });
}
