import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import type { MemoryListResponse } from "./types";

export const MEMORY_PAGE_SIZE = 20;

export const projectMemoryQueryKey = (repo: string, page: number) =>
  ["project-memory", repo, page] as const;

export function useProjectMemoryQuery(repo: string, page: number) {
  const offset = Math.max(0, page - 1) * MEMORY_PAGE_SIZE;
  return useQuery({
    queryKey: projectMemoryQueryKey(repo, page),
    queryFn: ({ signal }) =>
      apiFetch<MemoryListResponse>(
        `/projects/${encodeURIComponent(repo)}/memory?limit=${MEMORY_PAGE_SIZE}&offset=${offset}`,
        { signal },
      ),
    staleTime: 15_000,
    placeholderData: keepPreviousData,
    enabled: repo.length > 0,
  });
}

function memoryApiPath(repo: string, fingerprint: string): string {
  return `/projects/${encodeURIComponent(repo)}/memory/${encodeURIComponent(fingerprint)}`;
}

export function useSetMemoryDisabledMutation(repo: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ fingerprint, disabled }: { fingerprint: string; disabled: boolean }) =>
      apiFetch<void>(memoryApiPath(repo, fingerprint), {
        method: "PATCH",
        body: JSON.stringify({ disabled }),
      }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["project-memory", repo] }),
  });
}

export function useDeleteMemoryMutation(repo: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (fingerprint: string) =>
      apiFetch<void>(memoryApiPath(repo, fingerprint), { method: "DELETE" }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["project-memory", repo] }),
  });
}
