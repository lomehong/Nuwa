/**
 * URL ⇄ filter-state conversion for the activity page.
 *
 * Like the issue list, the URL is the single source of truth: a reload or a
 * shared link reproduces kinds, repo, fingerprint, range, aggregation and
 * page. Parsing is forgiving so an unknown value can never produce a 422;
 * writing only emits non-default values so links stay short.
 */

/**
 * Event kinds the backend actually writes. Anything else (runner_*, sync,
 * reopened, pr_merged, pr_closed) is either not persisted or not covered by the
 * activity page, so offering it would only produce empty results.
 */
export const EVENT_KINDS = [
  "reported",
  "state_changed",
  "requeued",
  "pr_opened",
  "merged",
  "rejected",
  "ci_green",
  "ci_red",
  "ci_timeout",
  "decision",
  "memory_updated",
  "memory_deleted",
  "config_changed",
  "config_migrated",
  "restart_requested",
] as const;

/** System/audit records shown by the activity page's second tab. */
export const AUDIT_KINDS = [
  "config_changed",
  "config_migrated",
  "restart_requested",
] as const;

export type EventRange = "24h" | "7d" | "30d" | "all";

export const EVENT_RANGES: { value: EventRange; label: string; ms: number | null }[] = [
  { value: "24h", label: "最近 24 小时", ms: 86_400_000 },
  { value: "7d", label: "最近 7 天", ms: 7 * 86_400_000 },
  { value: "30d", label: "最近 30 天", ms: 30 * 86_400_000 },
  { value: "all", label: "全部", ms: null },
];

export interface EventFilters {
  kinds: string[];
  repo: string;
  fingerprint: string;
  range: EventRange;
  aggregate: boolean;
  page: number;
}

export const DEFAULT_EVENT_FILTERS: EventFilters = {
  kinds: [],
  repo: "",
  fingerprint: "",
  range: "24h",
  aggregate: false,
  page: 1,
};

export function parseEventFilters(search: URLSearchParams): EventFilters {
  const kinds = Array.from(
    new Set(
      (search.get("kind") ?? "")
        .split(",")
        .map((part) => part.trim())
        .filter((part): part is (typeof EVENT_KINDS)[number] =>
          (EVENT_KINDS as readonly string[]).includes(part),
        ),
    ),
  );
  const range = EVENT_RANGES.some((entry) => entry.value === search.get("range"))
    ? (search.get("range") as EventRange)
    : DEFAULT_EVENT_FILTERS.range;
  const page = Number.parseInt(search.get("page") ?? "1", 10);
  return {
    kinds,
    repo: search.get("repo")?.trim() ?? "",
    fingerprint: search.get("fingerprint")?.trim() ?? "",
    range,
    aggregate: search.get("aggregate") === "1",
    page: Number.isFinite(page) && page > 0 ? page : 1,
  };
}

/** Console URL for the filters, e.g. `/events?kind=reported&range=7d`. */
export function eventPagePath(filters: EventFilters): string {
  const params = new URLSearchParams();
  if (filters.kinds.length) params.set("kind", filters.kinds.join(","));
  if (filters.repo) params.set("repo", filters.repo);
  if (filters.fingerprint) params.set("fingerprint", filters.fingerprint);
  if (filters.range !== DEFAULT_EVENT_FILTERS.range) params.set("range", filters.range);
  if (filters.aggregate) params.set("aggregate", "1");
  if (filters.page > 1) params.set("page", String(filters.page));
  const query = params.toString();
  return query ? `/events?${query}` : "/events";
}

/**
 * API-relative query string for `GET /events`. `from_ms` is derived from the
 * range at request time so the query key can stay stable between polls.
 */
export function eventQueryString(filters: EventFilters, nowMs = Date.now()): string {
  const params = new URLSearchParams();
  if (filters.kinds.length) params.set("kind", filters.kinds.join(","));
  if (filters.repo) params.set("repo", filters.repo);
  if (filters.fingerprint) params.set("fingerprint", filters.fingerprint);
  const range = EVENT_RANGES.find((entry) => entry.value === filters.range);
  if (range?.ms) params.set("from_ms", String(nowMs - range.ms));
  if (filters.aggregate) params.set("aggregate", "1");
  params.set("page", String(filters.page));
  params.set("page_size", "50");
  return params.toString();
}
