import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

import type { EventsResponse } from "../features/events/types";
import type {
  OverviewResponse,
  TrendsResponse,
  TrendTotals,
} from "../features/overview/types";
import type { IssueDetailResponse, RunDto } from "../features/issue-detail/types";
import type { IssueListItem, IssueListResponse } from "../features/issues/types";
import type { ProjectSummary } from "../features/projects/types";
import type { RunnerDto, RunnerListResponse } from "../features/runners/types";
import type { RepoDto, RepoListResponse } from "../features/projects/registry";
import type {
  SettingDto,
  SettingsResponse,
  SystemResponse,
} from "../features/settings/types";
import type { TranscriptResponse } from "../features/transcript/types";
import type { MeResponse } from "../lib/auth";
import type { ProjectSkillsResponse, SkillDto } from "../features/skills/types";
import type { MemoryDto, MemoryListResponse } from "../features/memory/types";
import type {
  BenchmarkDetailResponse,
  BenchmarkResultDto,
  BenchmarkRunDto,
  BenchmarkRunsResponse,
  ScenarioMeta,
} from "../features/benchmarks/types";

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
      ignored: 0,
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

type TrendsOverrides = Partial<Omit<TrendsResponse, "totals">> & {
  totals?: Partial<TrendTotals>;
};

export function makeTrends(overrides: TrendsOverrides = {}): TrendsResponse {
  const base: TrendsResponse = {
    range: "14d",
    buckets: [],
    totals: {
      reports: 0,
      new_issues: 0,
      prs_opened: 0,
      merged: 0,
      rejected: 0,
      tokens: 0,
      ci_green: 0,
      ci_red: 0,
      median_time_to_pr_secs: null,
      false_positive_rate: null,
      review_latency_p50_secs: null,
      ci_red_rate: null,
    },
  };
  // Deep-merge totals: call sites usually override a few counters only, and
  // the generated type keeps every field required (they are always present in
  // the API response).
  return {
    ...base,
    ...overrides,
    totals: { ...base.totals, ...(overrides.totals ?? {}) },
  };
}

export function mockTrends(overrides: TrendsOverrides = {}) {
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
    compactions: 1,
    context_window: 128_000,
    messages: 42,
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

/** One project rollup row. */
export function makeProject(overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return {
    repo: "example.com/app",
    name: null,
    url: null,
    description: null,
    registered: false,
    states: {
      new: 0,
      triggered: 0,
      fixing: 0,
      pr_open: 0,
      merged: 0,
      rejected: 0,
      escalated: 0,
      ignored: 0,
    },
    attention: 0,
    worst_severity: null,
    queued: 0,
    running: 0,
    pr_open: 0,
    escalated: 0,
    ignored: 0,
    merged: 0,
    rejected: 0,
    runners: 0,
    runners_online: 0,
    prs_today: 0,
    pr_limit: 0,
    ci_green: 0,
    ci_red: 0,
    reports_24h: 0,
    tokens_24h: 0,
    last_signal_ms: 1_700_000_000_000,
    last_delivery_ms: null,
    last_pr_url: null,
    ...overrides,
  };
}

export function mockProjects(projects: ProjectSummary[] = []) {
  return http.get("/api/v1/projects", () => HttpResponse.json({ data: projects }));
}

/**
 * The overview route now reads three endpoints. Most shell/router tests only
 * care that the page renders, so register all of them together.
 */
export function mockOverviewResources(overrides: Partial<OverviewResponse> = {}) {
  return [mockOverview(overrides), mockTrends(), mockEvents(), mockProjects()];
}

function settingSpec(overrides: Partial<SettingDto> = {}): SettingDto {
  return {
    key: "NUWA_TEST",
    label: "Test setting",
    group: "runner",
    type: "string",
    default: "",
    options: [],
    secret: false,
    scope: "work",
    requires_restart: true,
    read_only: false,
    help: "Test help",
    min: null,
    max: null,
    ...overrides,
  };
}

/**
 * `GET /api/v1/settings` fixture. One representative key per group keeps every
 * section renderable, with the credentials/interval/secret cases the page
 * tests depend on (docs/09 §9.2 settings).
 */
export function makeSettings(overrides: Partial<SettingsResponse> = {}): SettingsResponse {
  return {
    schema: [
      settingSpec({
        key: "NUWA_GITHUB_TOKEN",
        label: "GitHub token",
        group: "credentials",
        type: "secret",
        secret: true,
        scope: "both",
        default: null,
        help: "GitHub 细粒度 PAT：Contents 读写 + Actions 只读",
      }),
      settingSpec({
        key: "NUWA_PROVIDER",
        label: "LLM Provider",
        group: "model",
        type: "enum",
        options: ["zai", "anthropic", "faux"],
        default: "anthropic",
        help: "修复 runner 使用的模型提供方",
      }),
      settingSpec({
        key: "NUWA_INTERVAL",
        label: "Poll interval",
        group: "runner",
        type: "int",
        default: "30",
        min: 1,
        max: 86_400,
        help: "轮询间隔（秒）",
      }),
      settingSpec({
        key: "NUWA_SERVE_UNIT",
        label: "Serve unit",
        group: "service",
        default: "nuwa-serve",
        scope: "serve",
        help: "serve 进程的 systemd unit",
      }),
      settingSpec({
        key: "NUWA_COOKIE_SECURE",
        label: "Cookie secure",
        group: "security",
        type: "enum",
        options: ["auto", "always", "never"],
        default: "auto",
        scope: "serve",
        help: "会话 cookie 的 Secure 策略",
      }),
      settingSpec({
        key: "NUWA_STUCK_AFTER_SECS",
        label: "Stuck floor",
        group: "ops",
        type: "int",
        default: "300",
        min: 60,
        max: 86_400,
        scope: "serve",
        help: "卡单判定下限（秒）",
      }),
      settingSpec({
        key: "NUWA_ENV_FILE",
        label: "Env file",
        group: "advanced",
        type: "path",
        default: "/opt/nuwa/nuwa.env",
        scope: "both",
        read_only: true,
        help: "env 文件路径（只读展示）",
      }),
    ],
    values: {
      NUWA_GITHUB_TOKEN: {
        set: true,
        last4: "1234",
        updated_at_ms: 1_700_000_000_000,
      },
      NUWA_PROVIDER: { value: "anthropic", source: "env" },
      NUWA_INTERVAL: { value: "30", source: "env" },
      NUWA_SERVE_UNIT: { value: "nuwa-serve", source: "env" },
      NUWA_COOKIE_SECURE: { value: "auto", source: "default" },
      NUWA_STUCK_AFTER_SECS: { value: "300", source: "default" },
      NUWA_ENV_FILE: { value: "/opt/nuwa/nuwa.env", source: "env" },
    },
    env_path: "/opt/nuwa/nuwa.env",
    env_exists: true,
    env_permissions: "600",
    needs_migration: false,
    restart_pending: false,
    effective_diff: {
      NUWA_INTERVAL: { file: "30", runtime: "60", source: "file" },
    },
    unknown_keys: ["LEGACY_KEY"],
    last_changes: [
      {
        key: "NUWA_MAX_TURNS",
        ts_ms: 1_700_000_000_000,
        actor: "owner",
      },
    ],
    ...overrides,
  };
}

export function mockSettings(overrides: Partial<SettingsResponse> = {}) {
  const body = makeSettings(overrides);
  return http.get("/api/v1/settings", () => HttpResponse.json(body));
}

/** `GET /api/v1/system` fixture: one active unit, one `n/a` (no systemd). */
export function makeSystem(overrides: Partial<SystemResponse> = {}): SystemResponse {
  return {
    version: "0.2.0",
    build_commit: "0123456",
    started_at_ms: 1_700_000_000_000,
    uptime_secs: 5_400,
    db_path: "/opt/nuwa/shanhaijing.db",
    db_size_bytes: 45_056,
    local_mode: false,
    services: [
      { unit: "nuwa-serve", active: "active" },
      { unit: "nuwa-work", active: "n/a" },
    ],
    ...overrides,
  };
}

export function mockSystem(overrides: Partial<SystemResponse> = {}) {
  const body = makeSystem(overrides);
  return http.get("/api/v1/system", () => HttpResponse.json(body));
}

/** One console registry row; identities contain `/` by design. */
export function makeRepo(overrides: Partial<RepoDto> = {}): RepoDto {
  return {
    repo: "example.com/app",
    name: "Example App",
    url: "https://example.com/app",
    description: "",
    created_at_ms: 1_700_000_000_000,
    updated_at_ms: 1_700_000_000_000,
    ...overrides,
  };
}

/** Static registry handler; defaults to an empty registry. */
export function mockRepos(repos: RepoDto[] = []) {
  const body: RepoListResponse = { data: repos };
  return http.get("/api/v1/repos", () => HttpResponse.json(body));
}

// ---- skills / memory / benchmarks (P1/P2 console surfaces) ----

export function makeSkill(overrides: Partial<SkillDto> = {}): SkillDto {
  return {
    name: "rust-cargo",
    version: 1,
    description: "Rust 编译/测试失败路径",
    languages: ["rust"],
    matches: ["cargo"],
    body: "# Rust 修复路径\n先读第一条 error。",
    body_truncated: false,
    source: "builtin",
    origin: "builtin:rust-cargo.md",
    effective: true,
    overrides_builtin: false,
    allowlisted: false,
    ...overrides,
  };
}

export function makeProjectSkills(
  overrides: Partial<ProjectSkillsResponse> = {},
): ProjectSkillsResponse {
  return {
    repo: "example.com/app",
    selection_mode: "auto",
    allowlist: [],
    repo_path: "/srv/app",
    skills: [makeSkill()],
    warnings: [],
    ...overrides,
  };
}

/** Wildcard path: repo identities contain encoded slashes. */
export function mockProjectSkills(body: ProjectSkillsResponse = makeProjectSkills()) {
  return http.get(/\/api\/v1\/projects\/.*\/skills$/, () => HttpResponse.json(body));
}

export function makeMemory(overrides: Partial<MemoryDto> = {}): MemoryDto {
  return {
    fingerprint: "fp-memory",
    title: "panic in Average",
    state: "merged",
    commit_sha: "0123456789abcdef",
    pr_url: "https://github.com/acme/app/pull/9",
    root_cause: "ROOT CAUSE: loop bound",
    change_stat: "calc.go | 2 +-",
    files: 1,
    recorded_at_ms: 1_700_000_000_000,
    disabled: false,
    ...overrides,
  };
}

export function makeMemoryList(overrides: Partial<MemoryListResponse> = {}): MemoryListResponse {
  return {
    data: [makeMemory()],
    total: 1,
    limit: 20,
    offset: 0,
    ...overrides,
  };
}

export function mockProjectMemory(body: MemoryListResponse = makeMemoryList()) {
  return http.get(/\/api\/v1\/projects\/.*\/memory(\?.*)?$/, () =>
    HttpResponse.json(body),
  );
}

export function makeScenario(overrides: Partial<ScenarioMeta> = {}): ScenarioMeta {
  return {
    name: "node-empty-average",
    title: "Node 空数组平均值为 NaN",
    description: "average([]) 返回 NaN；测试期望 0。",
    language: "node",
    failing_check: "node --test",
    expect_fixed: true,
    ...overrides,
  };
}

export function makeBenchmarkRun(overrides: Partial<BenchmarkRunDto> = {}): BenchmarkRunDto {
  return {
    id: "bench-1",
    started_at_ms: 1_700_000_000_000,
    finished_at_ms: 1_700_000_004_000,
    provider: "faux",
    model: "faux",
    mode: "faux",
    status: "completed",
    scenarios: 2,
    repaired: 1,
    claimed: 2,
    false_positive: 1,
    delivery_ok: 1,
    turns: 4,
    tokens_uncached: 200,
    tokens_cached: 0,
    duration_ms: 4_000,
    error: null,
    ...overrides,
  };
}

export function makeBenchmarkResult(
  overrides: Partial<BenchmarkResultDto> = {},
): BenchmarkResultDto {
  return {
    scenario: "node-empty-average",
    language: "node",
    expected_fixed: true,
    repaired: true,
    claimed_fixed: true,
    delivery_ok: true,
    matches_expectation: true,
    turns: 2,
    tokens_uncached: 100,
    tokens_cached: 0,
    duration_ms: 900,
    root_cause: "ROOT CAUSE: empty input",
    final_report: "ROOT CAUSE: empty input\nVERIFICATION: node --test",
    error: null,
    ...overrides,
  };
}

export function makeBenchmarks(
  overrides: Partial<BenchmarkRunsResponse> = {},
): BenchmarkRunsResponse {
  return {
    data: [makeBenchmarkRun()],
    scenarios: [makeScenario()],
    ...overrides,
  };
}

export function mockBenchmarks(body: BenchmarkRunsResponse = makeBenchmarks()) {
  return http.get("/api/v1/benchmarks", () => HttpResponse.json(body));
}

export function makeBenchmarkDetail(
  overrides: Partial<BenchmarkDetailResponse> = {},
): BenchmarkDetailResponse {
  return {
    run: makeBenchmarkRun(),
    results: [makeBenchmarkResult()],
    ...overrides,
  };
}

export function mockBenchmarkDetail(
  id: string,
  body: BenchmarkDetailResponse = makeBenchmarkDetail(),
) {
  return http.get(`/api/v1/benchmarks/${encodeURIComponent(id)}`, () => HttpResponse.json(body));
}
