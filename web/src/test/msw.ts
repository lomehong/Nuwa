import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

import type {
  EventsResponse,
  OverviewResponse,
  TrendsResponse,
} from "../features/overview/types";
import type { IssueDetailResponse, RunDto } from "../features/issue-detail/types";
import type { IssueListItem, IssueListResponse } from "../features/issues/types";
import type { TranscriptResponse } from "../features/transcript/types";
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

export function makeRun(overrides: Partial<RunDto> = {}): RunDto {
  return {
    id: "run-default",
    started_at_ms: 1_700_000_000_000,
    finished_at_ms: 1_700_000_060_000,
    phase: "done",
    outcome: "success",
    ci_verdict: "success",
    turns: 6,
    tokens_uncached: 17_000,
    tokens_cached: 500,
    provider: "zai",
    model: "glm-5.3-flash",
    root_cause: "Average indexed an empty slice.",
    change_stat: "calc.go | 4 ++--",
    branch: "agent/fix/fp-default",
    commit_sha: "0123456789abcdef",
    pr_url: "https://github.com/example/app/pull/9",
    pr_opened_at_ms: 1_700_000_050_000,
    verification_ok: true,
    verification_detail: "go test ./... ok",
    error: null,
    ...overrides,
  };
}

export function makeIssueDetail(
  overrides: Partial<IssueDetailResponse> = {},
): IssueDetailResponse {
  return {
    issue: makeIssue({ owner_runner_id: "runner-alpha" }),
    stack_sample:
      "panic: runtime error: index out of range [3] with length 3\n\t/app/calc.go:17 +0x8c",
    timeline: [
      {
        kind: "state_changed",
        detail: "claimed by runner",
        ts_ms: 1_700_000_010_000,
        actor: "runner",
        repo: "example.com/app",
      },
    ],
    runs: [makeRun()],
    similar: { count: 0, items: [] },
    ...overrides,
  };
}

/**
 * Detail handler for `/api/v1/issues/:fp`. Unless the test supplies its own
 * issue, the response fingerprint follows the requested path segment so
 * header/copy assertions stay honest.
 */
export function mockIssueDetail(overrides: Partial<IssueDetailResponse> = {}) {
  return http.get("/api/v1/issues/:fp", ({ params }) => {
    const requested = typeof params.fp === "string" ? params.fp : "fp-default";
    const body = makeIssueDetail(overrides);
    if (!overrides.issue) {
      body.issue = { ...body.issue, fingerprint: requested };
    }
    return HttpResponse.json(body);
  });
}

export function makeTranscript(
  overrides: Partial<TranscriptResponse> = {},
): TranscriptResponse {
  return {
    data: [],
    meta: {
      page: 1,
      page_size: 20,
      total: 0,
      truncated: false,
      empty: true,
    },
    ...overrides,
  };
}

/** Static transcript handler; dynamic tests register their own query-aware one. */
export function mockTranscript(overrides: Partial<TranscriptResponse> = {}) {
  const body = makeTranscript(overrides);
  return http.get("/api/v1/issues/:fp/transcript", () => HttpResponse.json(body));
}

/**
 * The overview route now reads three endpoints. Most shell/router tests only
 * care that the page renders, so register all of them together.
 */
export function mockOverviewResources(overrides: Partial<OverviewResponse> = {}) {
  return [mockOverview(overrides), mockTrends(), mockEvents()];
}
