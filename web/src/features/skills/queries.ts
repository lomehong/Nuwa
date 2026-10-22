import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import type { ProjectSkillsResponse } from "./types";

export const projectSkillsQueryKey = (repo: string) => ["project-skills", repo] as const;

export function useProjectSkillsQuery(repo: string) {
  return useQuery({
    queryKey: projectSkillsQueryKey(repo),
    queryFn: ({ signal }) =>
      apiFetch<ProjectSkillsResponse>(`/projects/${encodeURIComponent(repo)}/skills`, {
        signal,
      }),
    staleTime: 30_000,
    enabled: repo.length > 0,
  });
}
