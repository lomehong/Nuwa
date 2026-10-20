/**
 * Project rollup data hook.
 *
 * The project list is the console's primary navigation surface, so it polls
 * with the same rhythm as the runner roster (10s) and pauses in a hidden tab.
 */

import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { backoffInterval } from "../../lib/polling";
import type { ProjectsResponse } from "./types";

export const PROJECTS_POLL_MS = 10_000;

export const projectsQueryKey = ["projects"] as const;

export function useProjectsQuery() {
  return useQuery({
    queryKey: projectsQueryKey,
    queryFn: ({ signal }) => apiFetch<ProjectsResponse>("/projects", { signal }),
    staleTime: PROJECTS_POLL_MS,
    refetchInterval: backoffInterval(PROJECTS_POLL_MS),
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}
