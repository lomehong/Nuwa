import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

import type {
  EventsResponse,
  OverviewResponse,
  TrendsResponse,
} from "../features/overview/types";
import type { IssueListItem, IssueListResponse } from "../features/issues/types";
import type { MeResponse } from "../lib/auth";

/** One MSW server per test file; handlers are registered with `server.use`. */
export const server = setupServer();

export function mockMe(overrides: Partial<MeResponse> = {}) {
  const body: MeResponse = {
    authenticated: true,
    local_mode: false,
    expires_at_ms: 1_700_000_000_000,
    ...overrides,
  };
  return http.get("/api/v1/auth/me", () => HttpResponse.json(body));
}

export function mockUnauthorizedMe() {
  return http.get("/api/v1/auth/me", () =>
    HttpResponse.json(
      { error: { code: "unauthorized", message: "not logged in" } },
      { status: 401 },
    ),
  );
}

export function makeOverview(overrides: Partial<OverviewResponse> = {}): OverviewResponse {
  return {
    counts: {
      new: 0,
      triggered: 0,
      fixing: 0,
      pr_open: 0,
      merged: 0,
      rejected: 0,
      escalated: 0,
    },
    attention: [],
    active_runs: [],
    queue: { ready_count: 0, next: [] },
    health: {
      runners_total: 0,
      runners_online: 0,
      working: 0,
      idle: 0,
      standing_down: 0,
      error: 0,
      reports_today: 0,
      prs_today: 0,
    },
    ci_24h: { green: 0, red: 0, timeout: 0, pending: 0 },
    ...overrides,
  };
}

export function mockOverview(overrides: Partial<OverviewResponse> = {}) {
  const body = makeOverview(overrides);
  return http.get("/api/v1/overview", () => HttpResponse.json(body));
}

export function makeTrends(overrides: Partial<TrendsResponse> = {}): TrendsResponse {
  return {
    range: "14d",
    buckets: [],
    totals: {
      reports: 0,
      new_issues: 0,
      prs_opened: 0,
      merged: 0,
      rejected: 0,
      tokens: 0,
      median_time_to_pr_secs: null,
    },
    ...overrides,
  };
}

export function mockTrends(overrides: Partial<TrendsResponse> = {}) {
  const body = makeTrends(overrides);
  return http.get("/api/v1/metrics/trends", () => HttpResponse.json(body));
}

export function makeEvents(overrides: Partial<EventsResponse> = {}): EventsResponse {
  return {
    data: [],
    meta: { page: 1, page_size: 8, total: 0 },
    ...overrides,
  };
}

export function mockEvents(overrides: Partial<EventsResponse> = {}) {
  const body = makeEvents(overrides);
  return http.get("/api/v1/events", () => HttpResponse.json(body));
}

export function makeIssue(overrides: Partial<IssueListItem> = {}): IssueListItem {
  return {
    fingerprint: "fp-default",
    title: "Default issue",
    language: "go",
    repo: "example.com/app",
    severity: "medium",
    state: "new",
    occurrences: 1,
    pr_url: null,
    first_seen_ms: 1_700_000_000_000,
    last_seen_ms: 1_700_000_000_000,
    state_changed_at_ms: 1_700_000_000_000,
    owner_runner_id: null,
    ci_verdict: null,
    stuck: false,
    ...overrides,
  };
}

export function makeIssues(overrides: Partial<IssueListResponse> = {}): IssueListResponse {
  return {
    data: [],
    meta: { page: 1, page_size: 50, total: 0 },
    ...overrides,
  };
}

/** Static issues-list handler; tests override it when they need query-aware bodies. */
export function mockIssues(overrides: Partial<IssueListResponse> = {}) {
  const body = makeIssues(overrides);
  return http.get("/api/v1/issues", () => HttpResponse.json(body));
}

/**
 * The overview route now reads three endpoints. Most shell/router tests only
 * care that the page renders, so register all of them together.
 */
export function mockOverviewResources(overrides: Partial<OverviewResponse> = {}) {
  return [mockOverview(overrides), mockTrends(), mockEvents()];
}
