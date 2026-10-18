/**
 * Wire shapes for `GET /api/v1/events` (`crates/nuwa/src/api/events.rs`).
 * Raw rows carry `count=1` and equal timestamps; aggregated `reported` rows
 * carry the group size and hour bounds.
 */

/** One row of `GET /events`; raw rows carry `count=1` and equal timestamps. */
export interface EventItem {
  fingerprint: string;
  kind: string;
  detail: string;
  ts_ms: number;
  repo: string;
  actor: string | null;
  count: number;
  first_ts_ms: number;
  last_ts_ms: number;
}

export interface PageMeta {
  page: number;
  page_size: number;
  total: number;
}

export interface EventsResponse {
  data: EventItem[];
  meta: PageMeta;
}
