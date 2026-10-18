# Web Console M4 — Read-only pages Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the read-only console: runners fleet page, activity/audit feed, and the 7/14/30/90-day trend switch, without any write actions.

**Architecture:** Three new frontend feature slices (`features/runners`, `features/events`, `features/overview` extension) consume the already-frozen M2b JSON endpoints. Shared event types move from `features/overview/types.ts` into `features/events/types.ts`. Routes and navigation become live for `/runners` and `/events`; `/settings` stays “Soon” until M5.

**Tech Stack:** React 19, TypeScript strict, Vite, Tailwind v4, TanStack Query v5, React Router v7, Vitest + RTL + MSW, lucide-react.

**Spec:** `docs/09-web-console-design.md` §9.2, §10.3–10.7, §10.9, §11, §12, §14 M4

## Global Constraints

- No backend/API changes in M4; the M2b contracts are frozen.
- No write actions (reopen/requeue/sync/restart/settings) in M4 — those are M5.
- No inline styles/scripts (strict CSP); dynamic geometry uses SVG attributes, not `style`.
- Every list supports loading, empty, stale/error and retry states; no full-screen spinner.
- Polling: runners 10s, events 30s, trends 60s; pause when the tab is hidden and refetch on focus.
- Touch controls stay ≥44px on coarse pointers (`min-h-11 pointer-fine:min-h-9`, `min-w-11` for icon buttons).
- `cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings` zero warnings; `npm run typecheck && npm run lint && npm test && npm run build` green.
- All work stays local on `main`; no push/tag/deploy.

---

## File structure

| File | Responsibility |
|---|---|
| `web/src/features/events/types.ts` | `EventItem`, `PageMeta`, `EventsResponse` (moved from overview) |
| `web/src/features/events/filters.ts` | URL ⇄ filter state, query path, kind catalog |
| `web/src/features/events/queries.ts` | `useEventsQuery` (30s poll) |
| `web/src/features/events/EventsPage.tsx` | Filter bar, audit preset, feed rows, pagination |
| `web/src/features/events/EventFilters.tsx` | Kind chips, repo/fingerprint inputs, range/aggregate controls |
| `web/src/features/events/EventRow.tsx` | System label vs issue link, kind badge, timestamps, detail |
| `web/src/features/runners/types.ts` | `RunnerDto`, `EffectiveConfig`, response |
| `web/src/features/runners/format.ts` | Status semantics, phase/config labels, heartbeat age |
| `web/src/features/runners/queries.ts` | `useRunnersQuery` (10s poll) |
| `web/src/features/runners/RunnersPage.tsx` | Summary strip, cards, empty state |
| `web/src/features/runners/RunnerCard.tsx` | One runner: identity, status, current issue, quota, config |
| `web/src/app/AppShell.tsx` | Activate nav items, page metadata, overview links |
| `web/src/app/router.tsx` | `/runners`, `/events` routes |
| `web/src/features/overview/TrendsPanel.tsx` | Range switch (7/14/30/90d) |
| `web/src/features/overview/OverviewPage.tsx` | Range state, runner/events links |
| `web/src/test/msw.ts` | `mockRunners`, `mockEvents` factories |

---

### Task 1: Routes, navigation and page metadata

**Files:**
- Modify: `web/src/app/router.tsx`
- Modify: `web/src/app/AppShell.tsx`
- Test: `web/src/app/AppShell.test.tsx`

**Interfaces:**
- Produces: routes `/runners` and `/events`; `pageMeta` titles `运行器` and `活动`.
- Consumes: `RunnersPage`, `EventsPage` default exports from Tasks 3 and 6 (imports added in this task; use placeholder components created here and replaced later).

- [ ] **Step 1: Write the failing shell test**

Add to `AppShell.test.tsx`:

```tsx
it("activates runners and events navigation and marks settings as soon", async () => {
  signedIn();
  renderApp("/");

  const nav = await screen.findByRole("navigation", { name: "主导航" });
  expect(within(nav).getByRole("link", { name: "运行器" })).toHaveAttribute(
    "href",
    "/runners",
  );
  expect(within(nav).getByRole("link", { name: "活动" })).toHaveAttribute("href", "/events");
  expect(within(nav).getByText("设置").closest("[aria-disabled='true']")).not.toBeNull();
  expect(within(nav).getAllByText("Soon")).toHaveLength(1);
});
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd web && npx vitest run src/app/AppShell.test.tsx -t "activates runners"`
Expected: FAIL because 运行器/活动 render as disabled spans.

- [ ] **Step 3: Implement**

In `AppShell.tsx`:

```ts
const NAV_ITEMS: readonly NavItem[] = [
  { label: "总览", icon: LayoutDashboard, to: "/", end: true },
  { label: "问题单", icon: ListTodo, to: "/issues" },
  { label: "运行器", icon: Cpu, to: "/runners" },
  { label: "活动", icon: Activity, to: "/events" },
  { label: "设置", icon: Settings, soon: "设置页面将在 M5 提供" },
];
```

Extend `pageMeta`:

```ts
if (pathname === "/runners" || pathname === "/runners/") {
  return { title: "运行器", breadcrumb: null };
}
if (pathname === "/events" || pathname === "/events/") {
  return { title: "活动", breadcrumb: null };
}
```

In `router.tsx` add routes inside the shell:

```tsx
<Route path="runners" element={<RunnersPage />} />
<Route path="events" element={<EventsPage />} />
```

Create minimal placeholders:

```tsx
// web/src/features/runners/RunnersPage.tsx
export function RunnersPage() {
  return <h2 className="text-title font-semibold text-text">运行器</h2>;
}
```

```tsx
// web/src/features/events/EventsPage.tsx
export function EventsPage() {
  return <h2 className="text-title font-semibold text-text">活动</h2>;
}
```

- [ ] **Step 4: Run the shell test**

Run: `cd web && npx vitest run src/app/AppShell.test.tsx`
Expected: PASS (the existing “Soon” count assertion must be updated from 3 to 1).

- [ ] **Step 5: Commit**

```bash
git add web/src/app/AppShell.tsx web/src/app/router.tsx web/src/app/AppShell.test.tsx web/src/features/runners/RunnersPage.tsx web/src/features/events/EventsPage.tsx
git commit -m "feat(web): activate runners and events routes"
```

---

### Task 2: Runners data layer and status semantics

**Files:**
- Create: `web/src/features/runners/types.ts`
- Create: `web/src/features/runners/format.ts`
- Create: `web/src/features/runners/queries.ts`
- Create: `web/src/features/runners/format.test.ts`
- Modify: `web/src/test/msw.ts`

**Interfaces:**
- Produces: `RunnerDto`, `RunnerListResponse`, `runnerStatusLabel(runner, nowMs)`, `runnerStatusVariant(runner)`, `runnerQuotaPercent(runner)`, `useRunnersQuery()`.
- Consumes: `GET /api/v1/runners` response from `crates/nuwa/src/api/runners.rs`.

- [ ] **Step 1: Write the failing format test**

```ts
import { describe, expect, it } from "vitest";
import { makeRunner } from "../../test/msw";
import { runnerQuotaPercent, runnerStatusLabel, runnerStatusVariant } from "./format";

const NOW = 1_700_000_000_000;

describe("runner status", () => {
  it("marks a stale heartbeat offline regardless of the stored status", () => {
    const runner = makeRunner({ status: "working", heartbeat_at_ms: NOW - 91_000 });
    expect(runnerStatusLabel(runner, NOW)).toBe("离线");
    expect(runnerStatusVariant(runner, NOW)).toBe("danger");
  });

  it("maps live statuses to operator vocabulary", () => {
    expect(runnerStatusLabel(makeRunner({ status: "starting" }), NOW)).toBe("启动中");
    expect(runnerStatusLabel(makeRunner({ status: "idle" }), NOW)).toBe("待命");
    expect(runnerStatusLabel(makeRunner({ status: "standing_down" }), NOW)).toBe("配额待命");
    expect(runnerStatusLabel(makeRunner({ status: "error" }), NOW)).toBe("异常");
    expect(runnerStatusLabel(makeRunner({ status: "working" }), NOW)).toBe("工作中");
  });

  it("computes the quota percentage without dividing by zero", () => {
    expect(runnerQuotaPercent(makeRunner({ prs_today: 2, daily_pr_limit: 5 }))).toBe(40);
    expect(runnerQuotaPercent(makeRunner({ prs_today: 2, daily_pr_limit: null }))).toBeNull();
    expect(
      runnerQuotaPercent(makeRunner({ prs_today: 9, daily_pr_limit: 5 })),
    ).toBe(100);
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd web && npx vitest run src/features/runners/format.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement types, format, queries**

`types.ts` mirrors the API exactly:

```ts
export interface EffectiveConfig {
  interval_secs: number | null;
  ci_timeout_secs: number | null;
  max_turns: number | null;
  max_tokens: number | null;
  failing_check: string | null;
  forbidden_paths_count: number | null;
}

export interface RunnerDto {
  id: string;
  repo: string;
  host: string;
  pid: number | null;
  started_at_ms: number;
  heartbeat_at_ms: number;
  status: string;
  offline: boolean;
  current_fingerprint: string | null;
  phase: string | null;
  provider: string | null;
  model: string | null;
  daily_pr_limit: number | null;
  prs_today: number;
  repo_path: string | null;
  service_unit: string | null;
  last_error: string | null;
  effective: EffectiveConfig;
}

export interface RunnerListResponse {
  data: RunnerDto[];
}
```

`format.ts`:

```ts
import type { BadgeVariant } from "../../components/ui";
import type { RunnerDto } from "./types";

export const RUNNER_OFFLINE_MS = 90_000;

const LABELS: Record<string, string> = {
  starting: "启动中",
  working: "工作中",
  idle: "待命",
  standing_down: "配额待命",
  error: "异常",
};

export function runnerIsOffline(runner: RunnerDto, nowMs: number): boolean {
  return nowMs - runner.heartbeat_at_ms > RUNNER_OFFLINE_MS || runner.offline;
}

export function runnerStatusLabel(runner: RunnerDto, nowMs: number): string {
  if (runnerIsOffline(runner, nowMs)) {
    return "离线";
  }
  return LABELS[runner.status] ?? runner.status;
}

export function runnerStatusVariant(runner: RunnerDto, nowMs: number): BadgeVariant {
  if (runnerIsOffline(runner, nowMs) || runner.status === "error") {
    return "danger";
  }
  switch (runner.status) {
    case "working":
      return "info";
    case "standing_down":
      return "warning";
    case "starting":
    case "idle":
      return "success";
    default:
      return "neutral";
  }
}

export function runnerQuotaPercent(runner: RunnerDto): number | null {
  if (!runner.daily_pr_limit || runner.daily_pr_limit <= 0) {
    return null;
  }
  return Math.min(100, Math.round((runner.prs_today / runner.daily_pr_limit) * 100));
}
```

`queries.ts`:

```ts
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "../../lib/api";
import type { RunnerListResponse } from "./types";

export const RUNNERS_POLL_MS = 10_000;

export function useRunnersQuery() {
  return useQuery({
    queryKey: ["runners"],
    queryFn: ({ signal }) => apiFetch<RunnerListResponse>("/runners", { signal }),
    staleTime: RUNNERS_POLL_MS,
    refetchInterval: RUNNERS_POLL_MS,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}
```

Add `makeRunner` and `mockRunners` to `test/msw.ts`:

```ts
import type { RunnerDto, RunnerListResponse } from "../features/runners/types";

export function makeRunner(overrides: Partial<RunnerDto> = {}): RunnerDto {
  return {
    id: "runner-1",
    repo: "example.com/app",
    host: "host-a",
    pid: 1234,
    started_at_ms: Date.now() - 3_600_000,
    heartbeat_at_ms: Date.now(),
    status: "idle",
    offline: false,
    current_fingerprint: null,
    phase: null,
    provider: "zai",
    model: "glm-5.3-flash",
    daily_pr_limit: 5,
    prs_today: 1,
    repo_path: "/srv/app",
    service_unit: "nuwa-work",
    last_error: null,
    effective: {
      interval_secs: 30,
      ci_timeout_secs: 600,
      max_turns: 30,
      max_tokens: 1_500_000,
      failing_check: "go test ./...",
      forbidden_paths_count: 3,
    },
    ...overrides,
  };
}

export function mockRunners(runners: RunnerDto[] = [makeRunner()]) {
  const body: RunnerListResponse = { data: runners };
  return http.get("/api/v1/runners", () => HttpResponse.json(body));
}
```

- [ ] **Step 4: Run the format test**

Run: `cd web && npx vitest run src/features/runners/format.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/runners web/src/test/msw.ts
git commit -m "feat(web): runner types, status semantics and query"
```

---

### Task 3: Runners page

**Files:**
- Create: `web/src/features/runners/RunnersPage.tsx`
- Create: `web/src/features/runners/RunnerCard.tsx`
- Create: `web/src/features/runners/RunnersPage.test.tsx`

**Interfaces:**
- Consumes: `useRunnersQuery`, `runnerStatusLabel/Variant`, `runnerQuotaPercent`, `useNow`? Use `Date.now()` at render; poll re-renders refresh it.
- Produces: the real `RunnersPage` used by Task 1.

- [ ] **Step 1: Write the failing page test**

```tsx
import { screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { makeRunner, mockMe, mockRunners, server } from "../../test/msw";
import { renderApp } from "../../test/render";
import { resetConnectivity } from "../../lib/connectivity";

afterEach(resetConnectivity);

describe("RunnersPage", () => {
  it("renders the fleet summary and one card per runner", async () => {
    server.use(
      mockMe(),
      mockRunners([
        makeRunner({ id: "runner-a", status: "working", current_fingerprint: "fp-1", phase: "repair" }),
        makeRunner({ id: "runner-b", status: "standing_down", prs_today: 5 }),
        makeRunner({ id: "runner-c", status: "error", last_error: "provider 401", offline: true }),
      ]),
    );

    renderApp("/runners");

    expect(await screen.findByRole("heading", { level: 2, name: "运行器" })).toBeInTheDocument();
    expect(screen.getByText("在线")).toBeInTheDocument();
    expect(screen.getByText("2/3")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /runner-a/ })).toBeInTheDocument();
    expect(within(screen.getByTestId("runner-card-runner-c")).getByText("离线")).toBeInTheDocument();
    expect(within(screen.getByTestId("runner-card-runner-b")).getByText("配额待命")).toBeInTheDocument();
    expect(screen.getByText("provider 401")).toBeInTheDocument();
  });

  it("shows a start command when no runner has registered", async () => {
    server.use(mockMe(), mockRunners([]));
    renderApp("/runners");

    expect(await screen.findByText("还没有运行器注册")).toBeInTheDocument();
    expect(screen.getByText(/nuwa work --loop --runner-id/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd web && npx vitest run src/features/runners/RunnersPage.test.tsx`
Expected: FAIL — heading-only placeholder, no cards.

- [ ] **Step 3: Implement the page**

`RunnersPage.tsx` structure:

```tsx
const summary = (runners: RunnerDto[]) => {
  const now = Date.now();
  const online = runners.filter((runner) => !runnerIsOffline(runner, now));
  return {
    online: online.length,
    total: runners.length,
    working: online.filter((runner) => runner.status === "working").length,
    idle: online.filter((runner) => runner.status === "idle").length,
    standingDown: online.filter((runner) => runner.status === "standing_down").length,
    error: online.filter((runner) => runner.status === "error").length,
  };
};
```

Render:
- `<h2>运行器</h2>` + description.
- A summary strip using `Badge`/`Card` with counts: 在线 `online/total`, 工作中, 待命, 配额待命, 异常.
- `runners.map((runner) => <RunnerCard key={runner.id} runner={runner} nowMs={now} />)` in `grid gap-4 md:grid-cols-2 xl:grid-cols-3`.
- Loading: skeleton cards with `role="status"`.
- Error: `PanelError`-style card with retry (`refetch`).
- Stale error: a `role="status"` line above the grid.
- Empty: `EmptyState` with title `还没有运行器注册`, description, and a `pre`/`code` block:
  `nuwa work --loop --runner-id <host:repo> --provider <provider>`.

`RunnerCard.tsx` must include:
- `data-testid={`runner-card-${runner.id}`}`.
- Header: `Cpu` icon, runner id as a copyable code link to `/issues/{current_fingerprint}` when present, repo/host, status `Badge`.
- Heartbeat: `最近心跳 {formatAge(heartbeat_at_ms)}`.
- Current work: fingerprint link + phase label, or `队列为空`.
- Provider/model, service unit, repo path.
- Quota: `<progress value={runner.prs_today} max={runner.daily_pr_limit ?? 1}>` with text `{prs_today}/{limit}`; `null` limit → `未上报配额`.
- `last_error` in a `danger` bordered paragraph.
- Copy buttons use the existing `CopyButton` (`features/issue-detail/CopyButton.tsx`) for id/service unit.
- M5 adds the restart button; M4 must not import it.

- [ ] **Step 4: Run the page test**

Run: `cd web && npx vitest run src/features/runners`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/runners
git commit -m "feat(web): runners fleet page"
```

---

### Task 4: Events types, URL filters and query

**Files:**
- Create: `web/src/features/events/types.ts`
- Create: `web/src/features/events/filters.ts`
- Create: `web/src/features/events/queries.ts`
- Create: `web/src/features/events/filters.test.ts`
- Modify: `web/src/features/overview/types.ts` (remove event types, re-export not needed)
- Modify: `web/src/features/overview/queries.ts`
- Modify: `web/src/features/overview/RecentEvents.tsx`
- Modify: `web/src/test/msw.ts`

**Interfaces:**
- Produces:
  - `EventItem`, `PageMeta`, `EventsResponse`.
  - `EventFilters` = `{ kinds: string[]; repo: string; fingerprint: string; range: EventRange; aggregate: boolean; page: number }`.
  - `parseEventFilters(search: URLSearchParams): EventFilters`
  - `eventQueryString(filters): string` (API-relative query without `/events`)
  - `eventPagePath(filters): string` (`/events?...`)
  - `useEventsQuery(filters)`.
- Consumes: `GET /api/v1/events` (`kind`, `repo`, `fingerprint`, `from_ms`, `to_ms`, `aggregate`, `page`, `page_size`).

- [ ] **Step 1: Write the failing filter tests**

```ts
import { describe, expect, it } from "vitest";
import { eventPagePath, parseEventFilters } from "./filters";

describe("event filters", () => {
  it("round-trips kinds, repo, range, aggregate and page", () => {
    const filters = parseEventFilters(
      new URLSearchParams(
        "kind=reported,state_changed&repo=example.com%2Fapp&fingerprint=abc&range=7d&aggregate=1&page=3",
      ),
    );
    expect(filters).toEqual({
      kinds: ["reported", "state_changed"],
      repo: "example.com/app",
      fingerprint: "abc",
      range: "7d",
      aggregate: true,
      page: 3,
    });
    expect(eventPagePath(filters)).toBe(
      "/events?kind=reported%2Cstate_changed&repo=example.com%2Fapp&fingerprint=abc&range=7d&aggregate=1&page=3",
    );
  });

  it("drops unknown kinds and falls back to 24h/page 1", () => {
    const filters = parseEventFilters(new URLSearchParams("kind=bogus&range=nope&page=0"));
    expect(filters.kinds).toEqual([]);
    expect(filters.range).toBe("24h");
    expect(filters.page).toBe(1);
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd web && npx vitest run src/features/events/filters.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

Move the three interfaces verbatim from `overview/types.ts` to `events/types.ts`; update imports in `overview/queries.ts`, `overview/RecentEvents.tsx`, and `test/msw.ts` to `../events/types`.

`filters.ts`:

```ts
export const EVENT_KINDS = [
  "reported",
  "state_changed",
  "requeued",
  "reopened",
  "pr_opened",
  "merged",
  "rejected",
  "ci_green",
  "ci_red",
  "ci_timeout",
  "config_changed",
  "config_migrated",
  "restart_requested",
  "runner_started",
  "runner_stopped",
  "runner_error",
  "sync",
] as const;

export const AUDIT_KINDS = [
  "config_changed",
  "config_migrated",
  "restart_requested",
  "runner_started",
  "runner_stopped",
  "runner_error",
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
  const kinds = (search.get("kind") ?? "")
    .split(",")
    .map((part) => part.trim())
    .filter((part): part is (typeof EVENT_KINDS)[number] =>
      (EVENT_KINDS as readonly string[]).includes(part),
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
```

`queries.ts` mirrors the other hooks (30s poll, focus refetch).

- [ ] **Step 4: Run the filter tests and the existing overview tests**

Run:
```bash
cd web && npx vitest run src/features/events/filters.test.ts src/features/overview
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/events web/src/features/overview web/src/test/msw.ts
git commit -m "feat(web): event types, URL filters and query"
```

---

### Task 5: Events page UI

**Files:**
- Create: `web/src/features/events/EventsPage.tsx` (replace placeholder)
- Create: `web/src/features/events/EventFilters.tsx`
- Create: `web/src/features/events/EventRow.tsx`
- Create: `web/src/features/events/EventsPage.test.tsx`

**Interfaces:**
- Consumes: `useEventsQuery`, filters helpers, `isSystemFingerprint`, `eventKindLabel`, `formatAge`, `formatDateTime`, `shortFingerprint`.
- Produces: the real `EventsPage` used by Task 1.

- [ ] **Step 1: Write the failing page test**

Register a dynamic MSW handler:

```tsx
it("syncs filters to the URL and renders system events without links", async () => {
  let requested = "";
  server.use(
    mockMe(),
    http.get("/api/v1/events", ({ request }) => {
      requested = new URL(request.url).search;
      return HttpResponse.json({
        data: [
          {
            fingerprint: "__config__",
            kind: "config_changed",
            detail: "NUWA_INTERVAL",
            ts_ms: Date.now() - 60_000,
            repo: "example.com/app",
            actor: "owner",
            count: 1,
            first_ts_ms: Date.now() - 60_000,
            last_ts_ms: Date.now() - 60_000,
          },
          {
            fingerprint: "abc123",
            kind: "state_changed",
            detail: "new -> triggered",
            ts_ms: Date.now() - 120_000,
            repo: "example.com/app",
            actor: "runner",
            count: 1,
            first_ts_ms: Date.now() - 120_000,
            last_ts_ms: Date.now() - 120_000,
          },
        ],
        meta: { page: 1, page_size: 50, total: 2 },
      });
    }),
  );

  renderApp("/events");
  expect(await screen.findByRole("heading", { level: 2, name: "活动" })).toBeInTheDocument();
  expect(screen.getByText("系统 · 配置")).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: /__config__/ })).not.toBeInTheDocument();
  expect(screen.getByRole("link", { name: "abc123…" })).toHaveAttribute(
    "href",
    "/issues/abc123",
  );

  fireEvent.click(screen.getByRole("button", { name: "配置审计" }));
  await waitFor(() => expect(requested).toContain("kind=config_changed"));
  expect(screen.getByText("审计视图")).toBeInTheDocument();
});
```

`MemoryRouter` does not change `window.location`; assert the page's filter state through the request and a visible “审计视图” badge instead.

- [ ] **Step 2: Run it and watch it fail**

Run: `cd web && npx vitest run src/features/events/EventsPage.test.tsx`
Expected: FAIL — placeholder page.

- [ ] **Step 3: Implement**

`EventsPage`:
- `useSearchParams()`; `filters = parseEventFilters(searchParams)`.
- `const setFilters = (patch: Partial<EventFilters>) => setSearchParams(new URLSearchParams(eventPagePath({ ...filters, ...patch }).split("?")[1] ?? ""))`.
- Query: `useEventsQuery(filters)`.
- Heading + description + `SyncButton` placeholder slot (M5).
- `<EventFilters filters={filters} onChange={setFilters} />`.
- Error/loading/empty states. Empty copy: “这个时间窗内没有事件。调整类型或时间范围，或等待新的上报/运行记录。”
- Rows in a `Card`/`Table` with columns: 时间, 类型, 问题单, 详情, 仓库, 操作者. At `<768px` render stacked cards; use the same responsive pattern as `IssueCards`.
- Pagination using the existing `IssuePagination` if its props match (`page`, `pageCount`, `onPageChange`, `fetching`); otherwise create `EventPagination` with the same markup.

`EventFilters`:
- Kind toggle chips for `EVENT_KINDS` (scrollable row on mobile); selected chips `aria-pressed`.
- “配置审计” preset sets `kinds: [...AUDIT_KINDS]`, `range: "30d"`, `page: 1`.
- Search inputs for repo and fingerprint; commit on `Enter`/blur.
- Range `Select` (`EVENT_RANGES`).
- Aggregate checkbox labeled “聚合历史上报（按小时）”.
- “清除筛选” resets to defaults.

`EventRow`:
- Kind label via `eventKindLabel` (already in `overview/format.ts`).
- System fingerprints: render a `Badge` `系统 · 配置` / `系统 · 运行器` / `系统` with no link. Reuse `isSystemFingerprint`.
- Non-system fingerprint: link to `/issues/{fingerprint}` with `shortFingerprint`.
- Aggregated rows show `×{count}` and the hour range (`formatDateTime(first_ts_ms)` – `formatDateTime(last_ts_ms)`).
- `detail` in monospace, wrapped, max 2 lines? Use `break-words`.

- [ ] **Step 4: Run the page tests**

Run: `cd web && npx vitest run src/features/events`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/events
git commit -m "feat(web): activity and audit feed"
```

---

### Task 6: Trend range switch and overview deep links

**Files:**
- Modify: `web/src/features/overview/TrendsPanel.tsx`
- Modify: `web/src/features/overview/OverviewPage.tsx`
- Modify: `web/src/features/overview/HealthPanel.tsx`
- Modify: `web/src/features/overview/RecentEvents.tsx`
- Test: `web/src/features/overview/OverviewPage.test.tsx`

**Interfaces:**
- `TrendsPanelProps` gains `range: TrendsRange` and `onRangeChange: (range: TrendsRange) => void`.
- Overview uses `useState<TrendsRange>("14d")` and `useTrendsQuery(range)`.
- Health panel links to `/runners`; recent events links to `/events`.

- [ ] **Step 1: Write the failing test**

```tsx
it("refetches trends for the selected range", async () => {
  signedIn();
  const ranges: string[] = [];
  server.use(
    http.get("/api/v1/metrics/trends", ({ request }) => {
      const range = new URL(request.url).searchParams.get("range") ?? "14d";
      ranges.push(range);
      return HttpResponse.json(makeTrends({
        range,
        buckets: [{ day: "2026-10-18", reports: 3, new_issues: 1, prs_opened: 0, merged: 0, rejected: 0, tokens: 10, ci_green: 0, ci_red: 0 }],
        totals: { reports: 3, new_issues: 1, prs_opened: 0, merged: 0, rejected: 0, tokens: 10, median_time_to_pr_secs: null },
      }));
    }),
  );

  renderApp("/");
  await screen.findByText("最近 14 天 · 按 UTC 日分桶");
  fireEvent.click(screen.getByRole("button", { name: "30 天" }));
  expect(await screen.findByText("最近 30 天 · 按 UTC 日分桶")).toBeInTheDocument();
  expect(ranges).toContain("30d");
});
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd web && npx vitest run src/features/overview/OverviewPage.test.tsx -t "trends for the selected range"`
Expected: FAIL — no 30-day button.

- [ ] **Step 3: Implement**

In `TrendsPanel`:
- Description: `最近 ${days} 天 · 按 UTC 日分桶`.
- Aside: a segmented `div role="group" aria-label="趋势时间范围"` with four buttons `7 天/14 天/30 天/90 天`, `aria-pressed={range === value}`, classes matching the issues filter chips (min-h-11 on touch).
- `onRangeChange` on click.
- Empty copy uses the selected day count.

In `OverviewPage`:
```tsx
const [range, setRange] = useState<TrendsRange>("14d");
const trends = useTrendsQuery(range);
// ...
<TrendsPanel range={range} onRangeChange={setRange} ... />
```

In `HealthPanel` add a link `查看运行器` to `/runners` in the section aside; in `RecentEvents` add `查看全部` to `/events`.

- [ ] **Step 4: Run overview tests**

Run: `cd web && npx vitest run src/features/overview`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/overview
git commit -m "feat(web): trend range switch and page deep links"
```

---

### Task 7: Verification, docs and UAT

**Files:**
- Modify: `docs/07-open-items.md`
- (No code changes expected; fix anything verification finds.)

- [ ] **Step 1: Run the full frontend suite**

```bash
cd web
npm run typecheck
npm run lint
npm test
npm run build
```
Expected: 28+ files pass; build writes `dist/index.html`.

- [ ] **Step 2: Run self-owned Rust checks**

```bash
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p nuwa -p shanhaijing
```
Expected: zero warnings; 197 + 60 tests pass (no backend changes).

- [ ] **Step 3: Browser UAT**

Use a temp DB (`nuwa ingest` + seeded runners/events) and `NUWA_UI_TOKEN=<dummy>`:
- `/runners`: summary, one card per status, offline override, quota, empty state.
- `/events`: default 24h, kind chips, audit preset, aggregate, pagination, system label no link.
- `/`: trend 7/14/30/90 switch, runner/events deep links.
- 390/1024/1440, light/dark, keyboard only.
- Zero horizontal overflow, zero inline styles, zero console errors.

- [ ] **Step 4: Update docs**

Append to the M3 bullet in `docs/07-open-items.md` (or add an M4 bullet) with: runners/events pages, trend ranges, test counts, UAT evidence, and the remaining M5/M6 items.

- [ ] **Step 5: Commit**

```bash
git add docs/07-open-items.md
git commit -m "docs: record M4 console pages"
```

---

## Self-review

- Spec coverage: runners §10.6 ✅ (restart action deferred to M5), events §10.7 ✅, trends range §10.3.4 ✅, general states §10.9 ✅, responsive/touch §11.7 ✅.
- Deferred intentionally: all write actions, settings, command palette, exponential backoff, legacy removal (M5/M6).
- No placeholders; every task has a failing test, implementation, command and commit.
