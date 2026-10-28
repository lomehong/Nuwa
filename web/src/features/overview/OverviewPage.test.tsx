import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse, delay } from "msw";
import { describe, expect, it, vi } from "vitest";

import type { EventsResponse } from "../events/types";
import type {
  OverviewResponse,
  TrendBucket,
  TrendsResponse,
} from "./types";
import { overviewQueryKey } from "./types";
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
      ignored: 0,
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
      pr_slots_today: 4,
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
    // Escalated → reopen; the stuck fixing card → requeue.
    expect(
      within(attention).getByRole("button", { name: "重新打开：fp-escalat…" }),
    ).toBeInTheDocument();
    expect(
      within(attention).getByRole("button", { name: "重排：fp-stuck-0…" }),
    ).toBeInTheDocument();

    const runs = within(attention).getAllByRole("link", { name: /查看详情/ });
    expect(runs[0]).toHaveAttribute("href", "/issues/fp-escalated-001");

    const active = screen.getByRole("region", { name: "正在发生" });
    expect(within(active).getByRole("link", { name: "Crash on empty input" })).toHaveAttribute(
      "href",
      "/issues/fp-run-001",
    );
    expect(
      within(active).getByRole("link", { name: "查看详情：Crash on empty input" }),
    ).toHaveAttribute("href", "/issues/fp-run-001");
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
      within(health).getByRole("img", { name: "最近 24 小时 CI：通过 12，失败 1，超时 2" }),
    ).toBeInTheDocument();
    expect(within(health).getByText("当前待定")).toHaveTextContent(
      "当前待定1（处于 CI 阶段的运行，不计入 24 小时累计）",
    );
    expect(within(health).getByText("今日上报").parentElement).toHaveTextContent("今日上报12");
    expect(within(health).getByText("今日 PR").parentElement).toHaveTextContent("今日 PR2");
    expect(within(health).getByRole("link", { name: "查看运行器" })).toHaveAttribute(
      "href",
      "/runners",
    );

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
    const reportsTable = within(trends).getByRole("table", { name: "上报 每日数值（14 天）" });
    expect(reportsChart).toHaveAttribute("aria-describedby", reportsTable.id);
    expect(within(reportsTable).getByRole("rowheader", { name: "10-01" })).toBeInTheDocument();
    expect(within(reportsTable).getByRole("rowheader", { name: "10-14" })).toBeInTheDocument();
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
    expect(within(events).getByRole("link", { name: "查看全部" })).toHaveAttribute(
      "href",
      "/events?range=7d",
    );

    expect(container.querySelector("[style]")).toBeNull();
  });

  it("refetches trends for the selected range", async () => {
    const ranges: string[] = [];
    server.use(
      mockMe(),
      mockOverview(),
      mockEvents(),
      http.get("/api/v1/metrics/trends", ({ request }) => {
        const range = new URL(request.url).searchParams.get("range") ?? "14d";
        ranges.push(range);
        return HttpResponse.json(
          makeTrends({
            range,
            buckets: [
              {
                day: "2026-10-18",
                reports: 3,
                new_issues: 1,
                prs_opened: 0,
                merged: 0,
                rejected: 0,
                tokens: 10,
                ci_green: 0,
                ci_red: 0,
              },
            ],
            totals: {
              reports: 3,
              new_issues: 1,
              prs_opened: 0,
              merged: 0,
              rejected: 0,
              tokens: 10,
              median_time_to_pr_secs: null,
            },
          }),
        );
      }),
    );

    renderApp("/");
    await screen.findByText("最近 14 天 · 按 UTC 日分桶");
    expect(screen.getByRole("button", { name: "14 天" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await waitFor(() => expect(ranges).toContain("14d"));

    fireEvent.click(screen.getByRole("button", { name: "30 天" }));
    expect(await screen.findByText("最近 30 天 · 按 UTC 日分桶")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "30 天" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await waitFor(() => expect(ranges).toContain("30d"));
    expect(ranges[0]).toBe("14d");
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

    const trendsRegion = screen.getByRole("region", { name: "趋势" });
    expect(within(trendsRegion).getByText("最近 14 天 · 按 UTC 日分桶")).toBeInTheDocument();
    expect(within(trendsRegion).getByText("暂无趋势数据")).toBeInTheDocument();
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
    // The live region holds only the message; the retry button stays outside.
    expect(within(alert).queryByRole("button")).not.toBeInTheDocument();

    fail = false;
    fireEvent.click(
      within(alert.parentElement as HTMLElement).getByRole("button", { name: "重试" }),
    );

    expect(
      await screen.findByRole("region", { name: "需要你处理" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Average panics on empty slice")).toBeInTheDocument();
  });

  it("keeps the stale-data retry outside the status live region", async () => {
    let fail = false;
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

    const { queryClient } = renderApp("/");
    await screen.findByRole("region", { name: "需要你处理" });

    fail = true;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: overviewQueryKey(null) }).catch(() => undefined);
    });

    const message = await screen.findByText("总览更新失败，正在显示上次数据。");
    const status = message.closest<HTMLElement>('[role="status"]');
    if (!status) {
      throw new Error("stale-data banner is not inside a status region");
    }
    // The retry button lives outside the live region.
    expect(within(status).queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    // Cached content stays visible below the banner.
    expect(screen.getByText("Average panics on empty slice")).toBeInTheDocument();
  });

  it("polls the shared overview key once per interval", async () => {
    let overviewRequests = 0;
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      server.use(
        mockMe(),
        http.get("/api/v1/overview", () => {
          overviewRequests += 1;
          return HttpResponse.json(richOverview());
        }),
        mockTrends(richTrends()),
        mockEvents(richEvents()),
      );

      renderApp("/");
      await screen.findByRole("region", { name: "需要你处理" });
      expect(overviewRequests).toBe(1);

      await vi.advanceTimersByTimeAsync(10_500);
      // The shell health strip owns the 10s timer; the page observer only
      // shares the key, so each cycle issues exactly one request.
      expect(overviewRequests).toBe(2);

      await vi.advanceTimersByTimeAsync(10_500);
      expect(overviewRequests).toBe(3);
    } finally {
      vi.useRealTimers();
    }
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
