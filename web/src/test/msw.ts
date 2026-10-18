import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

import type { OverviewResponse } from "../features/overview/types";
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
