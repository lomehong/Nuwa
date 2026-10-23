/**
 * Response types for `GET /api/v1/issues/{fp}` (docs/09 §9.2, §10.5).
 *
 * Required fields come from the Rust DTOs via ts-rs
 * (`web/src/lib/api-types/`); the optional extras the UI already renders when
 * present (aggregated history, system events) are layered on top. `issue` is
 * the same narrowed row shape as the list endpoint.
 */

import type { IssueDetailResponse as ApiIssueDetailResponse } from "../../lib/api-types/IssueDetailResponse";
import type { RunDto } from "../../lib/api-types/RunDto";
import type { SimilarItem as ApiSimilarItem } from "../../lib/api-types/SimilarItem";
import type { SimilarIssues as ApiSimilarIssues } from "../../lib/api-types/SimilarIssues";
import type { TimelineItem as ApiTimelineItem } from "../../lib/api-types/TimelineItem";
import type { IssueListItem } from "../issues/types";

/** One lifecycle event on the issue timeline (newest first). */
export type TimelineItem = ApiTimelineItem & {
  /** Aggregated historical reports ("共 N 次"); absent on raw events. */
  count?: number;
  /**
   * Event owner when the server includes it. System fingerprints
   * (`__config__`, `__system__`, `__runner__:<id>`) never link to an issue.
   */
  fingerprint?: string;
};

export type { RunDto };

/** Same-title neighbour. Extra fields arrive when the server adds them. */
export type SimilarItem = ApiSimilarItem & {
  title?: string;
  repo?: string;
  severity?: string;
  occurrences?: number;
};

export type SimilarIssues = Omit<ApiSimilarIssues, "items"> & {
  items: SimilarItem[];
};

export type IssueDetailResponse = Omit<ApiIssueDetailResponse, "issue" | "timeline" | "similar"> & {
  issue: IssueListItem;
  timeline: TimelineItem[];
  similar: SimilarIssues;
};
