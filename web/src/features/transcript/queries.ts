/**
 * Transcript data hook. `run_id` is optional: without it the server serves the
 * newest run that actually has a session directory. Pagination is 20 messages
 * per page (server default/limit), and every key segment is part of the query
 * key so switching runs never flashes another run's messages.
 */

import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { apiFetch } from "../../lib/api";
import { backoffInterval } from "../../lib/polling";
import type { TranscriptResponse } from "./types";

export const TRANSCRIPT_PAGE_SIZE = 20;
export const TRANSCRIPT_STALE_MS = 15_000;

export function transcriptQueryKey(
  fingerprint: string,
  runId: string | null,
  page: number,
) {
  return ["issue-transcript", fingerprint, runId, page] as const;
}

export function transcriptApiPath(
  fingerprint: string,
  runId: string | null,
  page: number,
): string {
  const params = new URLSearchParams({
    page: String(Math.max(1, page)),
    page_size: String(TRANSCRIPT_PAGE_SIZE),
  });
  if (runId) {
    params.set("run_id", runId);
  }
  return `/issues/${encodeURIComponent(fingerprint)}/transcript?${params.toString()}`;
}

/** Optional behaviour for the transcript query. */
interface TranscriptQueryOptions {
  /** Poll cadence (ms) while the selected run is still in flight. */
  refetchIntervalMs?: number;
}

export function useTranscriptQuery(
  fingerprint: string,
  runId: string | null,
  page: number,
  options?: TranscriptQueryOptions,
) {
  return useQuery({
    queryKey: transcriptQueryKey(fingerprint, runId, page),
    queryFn: ({ signal }) =>
      apiFetch<TranscriptResponse>(transcriptApiPath(fingerprint, runId, page), { signal }),
    enabled: fingerprint.length > 0,
    staleTime: TRANSCRIPT_STALE_MS,
    refetchOnWindowFocus: true,
    // docs/09 §10.5: an in-flight run appends messages continuously, so the
    // live view polls (with the shared backoff); finished runs stay static.
    refetchInterval: options?.refetchIntervalMs
      ? backoffInterval(options.refetchIntervalMs)
      : false,
    // Keep the current page visible while the next one loads; the viewer
    // focuses a page heading once the new data settles.
    placeholderData: keepPreviousData,
  });
}
