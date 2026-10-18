import { fireEvent, screen, within } from "@testing-library/react";
import { http, HttpResponse, delay } from "msw";
import { describe, expect, it } from "vitest";

import type {
  EventsResponse,
  OverviewResponse,
  TrendBucket,
  TrendsResponse,
} from "./types";
import { makeEvents, makeOverview, makeTrends, mockEvents, mockMe, mockOverview, mockTrends, server } from "../../test/msw";
import { renderApp } from "../../test/render";
import { RUN_PHASES, runPhaseIndex } from "./ActiveRuns";

const NOW = Date.now();

function richOverview(): OverviewResponse {
  return makeOverview({
    counts: {
      new: 2,
      triggered: 1,
      fixing: 1,
      pr_open: 3,
      merged: 4,
      rejected: 1,
      escalated: 2,
    },
    attention: [
      {
        fingerprint: "fp-escalated-001",
        title: "Average panics on empty slice",
        state: "escalated",
        severity: "high",
        repo: "example.com/app",
        last_seen_ms: NOW - 5 * 60_000,
        stuck: false,
        owner_runner_id: "runner-alpha",
        reason_code: "repair_failed",
        reason_text: "修复 runner 失败，需要复核",
        pr_url: "https://github.com/example/app/pull/7",
      },
      {
        fingerprint: "fp-stuck-002",
        title: "Stuck issue",
        state: "fixing",
        severity: "critical",
        repo: "example.com/worker",
        last_seen_ms: NOW - 2 * 60 * 60_000,
        stuck: true,
        owner_runner_id: null,
        reason_code: "stuck",
        reason_text: "疑似卡单，runner 心跳已过期",
        pr_url: null,
      },
    ],
    active_runs: [
      {
        id: "run-1",
        fingerprint: "fp-run-001",
        title: "Crash on empty input",
        repo: "example.com/app",
        runner_id: "runner-alpha",
        phase: "verify",
        started_at_ms: NOW - 125_000,
        turns: 6,
        tokens: 17_500,
      },
    ],
    queue: {
      ready_count: 3,
      next: [
        { fingerprint: "fp-queue-1", title: "Queue one", severity: "medium", occurrences: 4 },
        { fingerprint: "fp-queue-2", title: "Queue two", severity: "low", occurrences: 2 },
        { fingerprint: "fp-queue-3", title: "Queue three", severity: "high", occurrences: 9 },
      ],
    },
    health: {
      runners_total: 4,
      runners_online: 3,
      working: 1,
      idle: 1,
      standing_down: 1,
      error: 0,
      reports_today: 12,
      prs_today: 2,
    },
    ci_24h: { green: 12, red: 1, timeout: 2, pending: 1 },
  });
}

function trendBuckets(): TrendBucket[] {
  return Array.from({ length: 14 }, (_, index) => ({
    day: `2026-10-${String(index + 1).padStart(2, "0")}`,
    reports: index + 1,
    new_issues: index % 3,
    prs_opened: index % 2,
    merged: index % 4 === 0 ? 1 : 0,
    rejected: index === 13 ? 1 : 0,
    tokens: (index + 1) * 1_000,
    ci_green: index % 2,
    ci_red: 0,
  }));
}

function richTrends(): TrendsResponse {
  const buckets = trendBuckets();
  return makeTrends({
    buckets,
    totals: {
      reports: 105,
      new_issues: 19,
      prs_opened: 7,
      merged: 4,
      rejected: 1,
      tokens: 105_000,
      median_time_to_pr_secs: 95,
    },
  });
}

function richEvents(): EventsResponse {
  return makeEvents({
    data: [
      {
        fingerprint: "fp-run-001",
        kind: "state_changed",
        detail: "claimed by runner-alpha",
        ts_ms: NOW - 60_000,
        repo: "example.com/app",
        actor: "runner",
        count: 1,
        first_ts_ms: NOW - 60_000,
        last_ts_ms: NOW - 60_000,
      },
      {
        fingerprint: "__config__",
        kind: "config_changed",
        detail: "NUWA_INTERVAL",
        ts_ms: NOW - 120_000,
        repo: "",
        actor: "owner",
        count: 1,
        first_ts_ms: NOW - 120_000,
        last_ts_ms: NOW - 120_000,
      },
      {
        fingerprint: "fp-escalated-001",
        kind: "ci_red",
        detail: "integration tests failed",
        ts_ms: NOW - 3 * 60_000,
        repo: "example.com/app",
        actor: "runner",
        count: 1,
        first_ts_ms: NOW - 3 * 60_000,
        last_ts_ms: NOW - 3 * 60_000,
      },
    ],
    meta: { page: 1, page_size: 8, total: 3 },
  });
}

describe("OverviewPage", () => {
  it("renders attention, active runs, health, funnel, trends and events", async () => {
    server.use(
      mockMe(),
      mockOverview(richOverview()),
      mockTrends(richTrends()),
      mockEvents(richEvents()),
    );

    const { container } = renderApp("/");

    const attention = await screen.findByRole("region", { name: "需要你处理" });
    expect(
      within(attention).getByRole("link", { name: "Average panics on empty slice" }),
    ).toHaveAttribute("href", "/issues/fp-escalated-001");
    expect(within(attention).getByText("修复 runner 失败，需要复核")).toBeInTheDocument();
    expect(within(attention).getByText("example.com/app")).toBeInTheDocument();
    expect(within(attention).getByText(/分钟前/)).toBeInTheDocument();
    expect(within(attention).getByText("疑似卡单")).toBeInTheDocument();
    expect(
      within(attention).getByRole("link", { name: "打开 PR" }),
    ).toHaveAttribute("href", "https://github.com/example/app/pull/7");

    const runs = within(attention).getAllByRole("link", { name: /查看详情/ });
    expect(runs[0]).toHaveAttribute("href", "/issues/fp-escalated-001");

    const active = screen.getByRole("region", { name: "正在发生" });
    expect(within(active).getByRole("link", { name: "Crash on empty input" })).toHaveAttribute(
      "href",
      "/issues/fp-run-001",
    );
    expect(within(active).getByText("6")).toBeInTheDocument();
    expect(within(active).getByText("17.5k")).toBeInTheDocument();
    expect(within(active).getByText(/2 分/)).toBeInTheDocument();
    const currentStep = within(active).getByText("验证").closest("li");
    expect(currentStep).toHaveAttribute("aria-current", "step");

    const health = screen.getByRole("region", { name: "系统健康" });
    expect(within(health).getByText("3/4")).toBeInTheDocument();
    expect(health.querySelector('[data-runner-tile="offline"]')).toHaveTextContent("离线1");
    expect(health.querySelector('[data-runner-tile="standing_down"]')).toHaveTextContent(
      "配额待命1",
    );
    expect(health.querySelector('[data-runner-tile="error"]')).toHaveTextContent("异常0");
    expect(
      within(health).getByRole("img", { name: /最近 24 小时 CI：通过 12，失败 1，超时 2，待定 1/ }),
    ).toBeInTheDocument();
    expect(within(health).getByText("今日上报").parentElement).toHaveTextContent("今日上报12");
    expect(within(health).getByText("今日 PR").parentElement).toHaveTextContent("今日 PR2");

    const funnel = screen.getByRole("region", { name: "状态漏斗" });
    expect(
      within(funnel).getByRole("link", { name: "已升级：2 条，查看问题单" }),
    ).toHaveAttribute("href", "/issues?state=escalated");
    expect(
      within(funnel).getByRole("link", { name: "PR 待合并：3 条，查看问题单" }),
    ).toHaveAttribute("href", "/issues?state=pr_open");

    const trends = screen.getByRole("region", { name: "趋势" });
    expect(within(trends).getByText("最近 14 天 · 按 UTC 日分桶")).toBeInTheDocument();
    const reportsChart = within(trends).getByRole("img", {
      name: /上报，最近 14 天共 105，峰值 14/,
    });
    expect(reportsChart.querySelectorAll("rect")).toHaveLength(14);
    expect(reportsChart).toHaveAttribute("tabindex", "0");
    expect(within(trends).getByText("tokens")).toBeInTheDocument();
    expect(within(trends).getByText("105.0k")).toBeInTheDocument();

    const events = screen.getByRole("region", { name: "最近活动" });
    expect(within(events).getByText("状态变更")).toBeInTheDocument();
    expect(within(events).getByText("claimed by runner-alpha")).toBeInTheDocument();
    expect(within(events).getByRole("link", { name: "fp-run-001" })).toHaveAttribute(
      "href",
      "/issues/fp-run-001",
    );
    expect(within(events).getByText("系统")).toBeInTheDocument();
    expect(within(events).queryByRole("link", { name: /__config__/ })).not.toBeInTheDocument();

    expect(container.querySelector("[style]")).toBeNull();
  });

  it("renders empty states and the queue preview when nothing is running", async () => {
    server.use(
      mockMe(),
      mockOverview(
        makeOverview({
          queue: {
            ready_count: 3,
            next: [
              {
                fingerprint: "fp-queue-1",
                title: "Queue one",
                severity: "medium",
                occurrences: 4,
              },
              {
                fingerprint: "fp-queue-2",
                title: "Queue two",
                severity: "low",
                occurrences: 2,
              },
              {
                fingerprint: "fp-queue-3",
                title: "Queue three",
                severity: "high",
                occurrences: 9,
              },
            ],
          },
        }),
      ),
      mockTrends(),
      mockEvents(),
    );

    renderApp("/");

    const attention = await screen.findByRole("region", { name: "需要你处理" });
    expect(within(attention).getByText("暂无需要你处理的事项")).toBeInTheDocument();

    const active = screen.getByRole("region", { name: "正在发生" });
    expect(within(active).getByText("当前没有活动运行")).toBeInTheDocument();
    expect(
      within(active).getByText("队列中有 3 条待领取，运行器下一次轮询会领取队首问题。"),
    ).toBeInTheDocument();
    expect(within(active).getByRole("link", { name: "Queue one" })).toHaveAttribute(
      "href",
      "/issues/fp-queue-1",
    );
    expect(within(active).getByText("9 次")).toBeInTheDocument();

    expect(within(screen.getByRole("region", { name: "趋势" })).getByText("暂无趋势数据"))
      .toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "最近活动" })).getByText("暂无活动记录"))
      .toBeInTheDocument();
  });

  it("shows loading skeletons while the overview is pending", async () => {
    server.use(
      mockMe(),
      http.get("/api/v1/overview", async () => {
        await delay("infinite");
        return HttpResponse.json(makeOverview());
      }),
      mockTrends(),
      mockEvents(),
    );

    renderApp("/");

    expect(await screen.findByText("正在加载总览")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "需要你处理" })).not.toBeInTheDocument();
  });

  it("shows a retryable error when the overview fails", async () => {
    let fail = true;
    server.use(
      mockMe(),
      http.get("/api/v1/overview", () =>
        fail
          ? HttpResponse.json(
              { error: { code: "internal", message: "store read failed" } },
              { status: 500 },
            )
          : HttpResponse.json(richOverview()),
      ),
      mockTrends(richTrends()),
      mockEvents(richEvents()),
    );

    renderApp("/");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("无法加载总览");
    expect(alert).toHaveTextContent("store read failed");
    expect(alert).toHaveTextContent("(internal)");

    fail = false;
    fireEvent.click(within(alert).getByRole("button", { name: "重试" }));

    expect(
      await screen.findByRole("region", { name: "需要你处理" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Average panics on empty slice")).toBeInTheDocument();
  });

  it("maps the active run phases onto the five-step stepper", () => {
    expect(RUN_PHASES.map((phase) => phase.id)).toEqual([
      "claim",
      "repair",
      "verify",
      "deliver",
      "ci",
    ]);
    expect(runPhaseIndex("claim")).toBe(0);
    expect(runPhaseIndex("repair")).toBe(1);
    expect(runPhaseIndex("verify")).toBe(2);
    expect(runPhaseIndex("deliver")).toBe(3);
    expect(runPhaseIndex("ci")).toBe(4);
    expect(runPhaseIndex("done")).toBe(-1);
  });
});
