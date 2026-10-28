import { QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { delay, http, HttpResponse } from "msw";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { describe, expect, it } from "vitest";

import { ToastProvider } from "../../components/ui";
import { makeIssue, makeIssueDetail, makeRun, makeSignalObservation, makeTranscript, mockIssueDetail, mockTranscript, server } from "../../test/msw";
import { createTestQueryClient } from "../../test/render";
import { formatDateTime, isoDateTime } from "./format";
import type { IssueDetailResponse } from "./types";
import { IssueDetailPage } from "./IssueDetailPage";

const NOW = Date.now();

function fixtureIssue() {
  return makeIssue({
    fingerprint: "fp-detail-001",
    title: "Average panics on empty slice",
    language: "go",
    repo: "example.com/app",
    severity: "critical",
    state: "escalated",
    occurrences: 17,
    pr_url: "https://github.com/example/app/pull/9",
    ci_verdict: "success",
    owner_runner_id: "runner-alpha",
    stuck: true,
    first_seen_ms: NOW - 3 * 24 * 60 * 60_000,
    last_seen_ms: NOW - 5 * 60_000,
    state_changed_at_ms: NOW - 2 * 60 * 60_000,
  });
}

function fixtureRuns() {
  const latest = makeRun({
    id: "run-latest",
    started_at_ms: NOW - 60 * 60_000,
    finished_at_ms: NOW - 59 * 60_000,
    phase: "done",
    outcome: "success",
    ci_verdict: "success",
    turns: 6,
    tokens_uncached: 17_000,
    tokens_cached: 500,
    root_cause: "Average indexed an empty slice at calc.go:17.",
    change_stat: "calc.go | 4 ++--\ncalc_test.go | 8 ++++++++",
    branch: "agent/fix/fp-detail-001",
    commit_sha: "0123456789abcdef",
    pr_url: "https://github.com/example/app/pull/9",
    pr_opened_at_ms: NOW - 58 * 60_000,
    verification_ok: true,
    verification_detail: "go test ./... ok\nok example.com/app 0.42s",
  });
  const older = makeRun({
    id: "run-old",
    started_at_ms: NOW - 4 * 60 * 60_000,
    finished_at_ms: NOW - 3 * 60 * 60_000,
    outcome: "ci_red",
    ci_verdict: "failure",
    turns: 9,
    tokens_uncached: 9_000,
    tokens_cached: 0,
    root_cause: null,
    change_stat: null,
    branch: "agent/fix/old",
    commit_sha: "fedcba9876543210",
    pr_url: null,
    pr_opened_at_ms: null,
    verification_ok: false,
    verification_detail: null,
    error: "CI failed on go test ./...",
  });
  return [latest, older];
}

function fixtureDetail(): IssueDetailResponse {
  return makeIssueDetail({
    issue: fixtureIssue(),
    stack_sample:
      "panic: runtime error: index out of range [3] with length 3\n\t/app/calc.go:17 +0x8c",
    timeline: [
      {
        kind: "state_changed",
        detail: "claimed by runner",
        ts_ms: NOW - 30 * 60_000,
        actor: "runner",
        repo: "example.com/app",
      },
      {
        kind: "reported",
        detail: "panic in Average",
        ts_ms: NOW - 90 * 60_000,
        actor: "ingest",
        repo: "example.com/app",
        count: 3,
      },
      {
        kind: "config_changed",
        detail: "NUWA_MODEL changed",
        ts_ms: NOW - 2 * 60 * 60_000,
        actor: "owner",
        repo: "example.com/app",
        fingerprint: "__config__",
      },
    ],
    runs: fixtureRuns(),
    ci_watch: {
      state: "pending",
      last_verdict: null,
      last_checked_at_ms: NOW - 30_000,
      next_check_at_ms: NOW + 30_000,
      check_attempts: 0,
      last_error: null,
    },
    similar: {
      count: 2,
      items: [
        {
          fingerprint: "fp-similar-a",
          state: "merged",
          last_seen_ms: NOW - 10 * 60_000,
          title: "Same crash in Average",
          repo: "example.com/app",
          severity: "medium",
          occurrences: 4,
        },
        {
          fingerprint: "fp-similar-b",
          state: "escalated",
          last_seen_ms: NOW - 20 * 60_000,
        },
      ],
    },
  });
}

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location">{`${location.pathname}${location.search}`}</div>;
}

function renderDetail(route = "/issues/fp-detail-001") {
  const queryClient = createTestQueryClient();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <MemoryRouter initialEntries={[route]}>
          <Routes>
            <Route
              path="/issues/:fp"
              element={
                <>
                  <IssueDetailPage />
                  <LocationProbe />
                </>
              }
            />
            <Route path="/issues" element={<h2>问题单列表</h2>} />
          </Routes>
        </MemoryRouter>
      </ToastProvider>
    </QueryClientProvider>,
  );
  return { ...result, queryClient };
}

function useDetail(detail: IssueDetailResponse) {
  server.use(
    http.get("/api/v1/issues/:fp", () => HttpResponse.json(detail)),
    http.get("/api/v1/issues/:fp/transcript", () =>
      HttpResponse.json(makeTranscript()),
    ),
  );
}

describe("IssueDetailPage", () => {
  it("renders the newest receipts with separate collector time, trust and text-only metadata", async () => {
    const receivedAt = NOW - 10_000;
    const observedAt = NOW - 30 * 60_000;
    const environment = '<img src=x onerror="alert(1)">';
    const release = "<b>0123456789abcdef</b>";
    const eventId = "<script>alert('event')</script>";
    const source = "<em>owner-import</em>";
    useDetail({
      ...fixtureDetail(),
      observations: [
        makeSignalObservation({
          id: 42,
          fingerprint: "fp-detail-001",
          received_at_ms: receivedAt,
          observed_at_ms: observedAt,
          trusted_source: source,
          time_status: "untrusted",
          disposition: "delayed",
          environment,
          release_sha: release,
          source_event_id: eventId,
        }),
        makeSignalObservation({
          id: 41,
          fingerprint: "fp-detail-001",
          received_at_ms: NOW - 60_000,
          observed_at_ms: NOW - 90_000,
          time_status: "trusted",
          environment: "staging",
        }),
      ],
    });
    renderDetail();

    const section = await screen.findByRole("region", { name: "信号回执" });
    expect(within(section).getByRole("heading", { level: 3, name: "信号回执" })).toBeInTheDocument();
    expect(within(section).getByText("最近 2 条")).toBeInTheDocument();
    const history = within(section).getByRole("region", { name: "回执记录" });
    const rows = within(history).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    // Collector time is older on the newest receipt; receipt order must win.
    expect(rows[0]).toHaveTextContent("回执 #42");
    expect(rows[1]).toHaveTextContent("回执 #41");
    const newest = within(rows[0]);
    expect(newest.getByText("接收时间")).toBeInTheDocument();
    expect(newest.getByText(formatDateTime(receivedAt))).toHaveAttribute("datetime", isoDateTime(receivedAt));
    expect(newest.getByText("采集时间")).toBeInTheDocument();
    expect(newest.getByText(formatDateTime(observedAt))).toHaveAttribute("datetime", isoDateTime(observedAt));
    expect(newest.getByText("来源已授信")).toBeInTheDocument();
    expect(newest.getByText("未授信")).toHaveAttribute("title", "untrusted");
    expect(newest.getByText("延迟上报")).toBeInTheDocument();
    for (const [label, value] of [
      ["环境", environment],
      ["发布版本 SHA", release],
      ["来源事件 ID", eventId],
      ["授信来源", source],
    ]) {
      expect(newest.getByText(label)).toBeInTheDocument();
      expect(newest.getByText(value)).toBeInTheDocument();
    }
    expect(within(rows[1]).getByText("来源未授信")).toBeInTheDocument();
    expect(within(rows[1]).getByText("已授信")).toHaveAttribute("title", "trusted");
    expect(history.querySelector("img, script, b, em")).toBeNull();
  });

  it("does not infer deployment or recovery from a merged issue with successful verification and CI", async () => {
    useDetail({
      ...fixtureDetail(),
      issue: { ...fixtureIssue(), state: "merged", stuck: false },
      observations: [makeSignalObservation({
        fingerprint: "fp-detail-001",
        environment: "production",
        release_sha: "0123456789abcdef",
        trusted_source: "owner-import",
        observed_at_ms: NOW - 30_000,
        time_status: "trusted",
      })],
      deployment_status: "unknown",
      recovery_status: "unknown",
    });
    renderDetail();

    const title = await screen.findByRole("heading", { level: 2, name: fixtureIssue().title });
    expect(within(title.closest("header") as HTMLElement).getByText("已合并")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "证据" })).getByText("CI 通过")).toBeInTheDocument();
    const history = screen.getByRole("region", { name: "信号回执" });
    expect(within(history).getByText("production")).toBeInTheDocument();
    expect(within(history).getByText("部署状态：未知")).toBeInTheDocument();
    expect(within(history).getByText("恢复状态：未知")).toBeInTheDocument();
    expect(within(history).getByText(/PR 已合并不等于已部署，也不等于运行时已恢复/)).toBeInTheDocument();
  });

  it.each(["empty", "missing"])("keeps legacy issue details usable with %s receipt history", async (history) => {
    const detail = { ...fixtureDetail(), observations: [] };
    server.use(
      http.get("/api/v1/issues/:fp", () => HttpResponse.json(history === "missing" ? {
        ...detail,
        observations: undefined,
        deployment_status: undefined,
        recovery_status: undefined,
      } : detail)),
      mockTranscript(),
    );
    renderDetail();

    const section = await screen.findByRole("region", { name: "信号回执" });
    expect(within(section).getByText("最近 0 条")).toBeInTheDocument();
    expect(within(section).getByText(/暂无信号回执；旧问题单或已过保留期/)).toBeInTheDocument();
    expect(within(section).queryByRole("region", { name: "回执记录" })).not.toBeInTheDocument();
    expect(within(section).getByText("部署状态：未知")).toBeInTheDocument();
    expect(within(section).getByText("恢复状态：未知")).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "时间线" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "证据" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "会话转录" })).toBeInTheDocument();
  });

  it("shows the last and next continuous CI check times", async () => {
    const detail = fixtureDetail();
    useDetail(detail);
    renderDetail();

    const watch = await screen.findByText(/CI 持续观察/);
    expect(watch.textContent).toContain("最近检查");
    expect(watch.textContent).toContain("下次检查");
    expect(watch.textContent).toContain(formatDateTime(NOW + 30_000));
  });

  it("renders the header, timeline, evidence, runs, stack and similar issues", async () => {
    const detail = fixtureDetail();
    useDetail(detail);
    const { container } = renderDetail();

    const title = await screen.findByRole("heading", {
      level: 2,
      name: "Average panics on empty slice",
    });
    const header = title.closest("header");
    expect(header).not.toBeNull();
    expect(within(header as HTMLElement).getByText("严重")).toBeInTheDocument();
    expect(within(header as HTMLElement).getByText("已升级")).toBeInTheDocument();
    expect(within(header as HTMLElement).getByText("疑似卡单")).toBeInTheDocument();
    expect(within(header as HTMLElement).getByText("example.com/app")).toBeInTheDocument();
    expect(within(header as HTMLElement).getByText("go")).toBeInTheDocument();
    expect(within(header as HTMLElement).getByText("17")).toBeInTheDocument();
    expect(within(header as HTMLElement).getAllByText("fp-detail-001").length).toBeGreaterThan(0);
    expect(
      within(header as HTMLElement).getByRole("button", {
        name: "复制完整指纹：fp-detail-001",
      }),
    ).toBeInTheDocument();
    expect(within(header as HTMLElement).getByTitle(formatDateTime(fixtureIssue().first_seen_ms))).toBeInTheDocument();
    expect(
      within(header as HTMLElement).getByRole("link", { name: /打开 PR/ }),
    ).toHaveAttribute("href", "https://github.com/example/app/pull/9");

    const timeline = screen.getByRole("region", { name: "时间线" });
    expect(within(timeline).getByText("claimed by runner")).toBeInTheDocument();
    expect(within(timeline).getByText("操作者：运行器")).toBeInTheDocument();
    expect(within(timeline).getByText("共 3 次")).toBeInTheDocument();
    expect(within(timeline).getByText("配置变更")).toBeInTheDocument();
    expect(within(timeline).getByText("系统事件")).toBeInTheDocument();
    expect(timeline.querySelector("time")).toHaveAttribute(
      "title",
      formatDateTime(NOW - 30 * 60_000),
    );

    const evidence = screen.getByRole("region", { name: "证据" });
    expect(
      within(evidence).getByText("Average indexed an empty slice at calc.go:17."),
    ).toBeInTheDocument();
    expect(within(evidence).getByText(/calc\.go \| 4 \+\+--/)).toBeInTheDocument();
    expect(within(evidence).getByText("验证")).toBeInTheDocument();
    expect(within(evidence).getByText("通过")).toBeInTheDocument();
    expect(within(evidence).getByText("CI 通过")).toBeInTheDocument();
    expect(within(evidence).getByText("agent/fix/fp-detail-001")).toBeInTheDocument();
    expect(within(evidence).getByText("0123456789abcdef")).toBeInTheDocument();

    const verification = within(evidence)
      .getByText("验证输出")
      .closest("summary") as HTMLElement;
    expect(verification.closest("details")).not.toHaveAttribute("open");
    fireEvent.click(verification);
    expect(verification.closest("details")).toHaveAttribute("open");
    expect(within(evidence).getByText(/go test \.\/\.\.\. ok/)).toBeInTheDocument();
    expect(
      within(evidence).getByRole("button", { name: "复制验证输出" }),
    ).toBeInTheDocument();

    const runs = screen.getByRole("region", { name: "运行记录" });
    expect(within(runs).getByText("成功")).toBeInTheDocument();
    expect(within(runs).getByText("CI 未通过")).toBeInTheDocument();
    expect(within(runs).getByText("17.5k")).toBeInTheDocument();
    expect(within(runs).getByText("agent/fix/fp-detail-001")).toBeInTheDocument();
    expect(within(runs).getByText("0123456789abcdef")).toBeInTheDocument();
    expect(within(runs).getByText(/CI failed on go test/)).toBeInTheDocument();
    expect(
      within(runs).getByRole("button", { name: "查看会话：run-old" }),
    ).toBeInTheDocument();

    const similar = screen.getByRole("region", { name: "相似问题" });
    expect(within(similar).getByText("共 2 个同名问题单，排除当前指纹。")).toBeInTheDocument();
    expect(
      within(similar).getByRole("link", { name: "Same crash in Average" }),
    ).toHaveAttribute("href", "/issues/fp-similar-a");
    expect(
      within(similar).getByRole("link", { name: "fp-similar…" }),
    ).toHaveAttribute("href", "/issues/fp-similar-b");

    const stack = screen.getByRole("region", { name: "堆栈样本" });
    expect(within(stack).getByText(/index out of range \[3\]/)).toBeInTheDocument();
    expect(
      within(stack).getByRole("button", { name: "复制堆栈样本" }),
    ).toBeInTheDocument();

    expect(container.querySelector("[style]")).toBeNull();
  });

  it("selects a run for the transcript and paginates it through the URL", async () => {
    const detail = fixtureDetail();
    const requests: URL[] = [];
    server.use(
      http.get("/api/v1/issues/:fp", () => HttpResponse.json(detail)),
      http.get("/api/v1/issues/:fp/transcript", ({ request }) => {
        const url = new URL(request.url);
        requests.push(url);
        const page = Number(url.searchParams.get("page") ?? "1");
        const runId = url.searchParams.get("run_id");
        return HttpResponse.json(
          makeTranscript({
            data: [
              {
                seq: (runId ? 100 : 0) + page,
                role: "assistant",
                ts_ms: NOW,
                blocks: [
                  {
                    type: "text",
                    text: runId ? `来自 ${runId} 第 ${page} 页` : `默认会话第 ${page} 页`,
                  },
                ],
              },
            ],
            meta: {
              page,
              page_size: 20,
              total: 45,
              truncated: false,
              empty: false,
            },
          }),
        );
      }),
    );

    renderDetail();

    expect(await screen.findByText("默认会话第 1 页")).toBeInTheDocument();
    expect(requests).toHaveLength(1);
    expect(requests[0].searchParams.get("run_id")).toBeNull();

    fireEvent.click(
      screen.getByRole("button", { name: "查看会话：run-old" }),
    );

    expect(await screen.findByText("来自 run-old 第 1 页")).toBeInTheDocument();
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("run_id")).toBe("run-old");
    expect(requests[1].searchParams.get("page")).toBe("1");
    expect(screen.getByTestId("location")).toHaveTextContent(
      "/issues/fp-detail-001?run_id=run-old",
    );
    expect(
      screen.getByRole("button", { name: "正在查看会话：run-old" }),
    ).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(screen.getByRole("button", { name: "下一页" }));

    expect(await screen.findByText("来自 run-old 第 2 页")).toBeInTheDocument();
    await waitFor(() => expect(requests).toHaveLength(3));
    expect(requests[2].searchParams.get("run_id")).toBe("run-old");
    expect(requests[2].searchParams.get("page")).toBe("2");
    expect(screen.getByTestId("location")).toHaveTextContent(
      "/issues/fp-detail-001?run_id=run-old&page=2",
    );

    // After the new page settles, focus lands on the page heading so keyboard
    // users continue reading at the top of the fresh message block.
    await waitFor(() =>
      expect(screen.getByTestId("transcript-page-heading")).toHaveFocus(),
    );
    expect(screen.getByTestId("transcript-page-heading")).toHaveTextContent(
      "会话消息 · 第 2 / 3 页",
    );

    fireEvent.click(
      screen.getByRole("button", { name: "查看会话：run-latest" }),
    );

    expect(await screen.findByText("来自 run-latest 第 1 页")).toBeInTheDocument();
    await waitFor(() => expect(requests).toHaveLength(4));
    expect(requests[3].searchParams.get("run_id")).toBe("run-latest");
    expect(requests[3].searchParams.get("page")).toBe("1");
  });

  it("shows the 404 state for an unknown fingerprint", async () => {
    server.use(
      http.get("/api/v1/issues/:fp", () =>
        HttpResponse.json(
          { error: { code: "not_found", message: "issue not found" } },
          { status: 404 },
        ),
      ),
    );

    renderDetail("/issues/missing-fp");

    expect(await screen.findByText("问题单不存在")).toBeInTheDocument();
    expect(
      screen.getByText(/未找到指纹 missing-fp 的问题单/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "返回问题单列表" }),
    ).toHaveAttribute("href", "/issues");
  });

  it("shows a loading skeleton while the detail request is pending", async () => {
    server.use(
      http.get("/api/v1/issues/:fp", async () => {
        await delay("infinite");
        return HttpResponse.json(fixtureDetail());
      }),
    );

    renderDetail();

    expect(screen.getByText("正在加载问题单详情")).toBeInTheDocument();
  });

  it("shows a retryable error and recovers", async () => {
    let fail = true;
    server.use(
      http.get("/api/v1/issues/:fp", () =>
        fail
          ? HttpResponse.json(
              { error: { code: "internal", message: "store read failed" } },
              { status: 500 },
            )
          : HttpResponse.json(fixtureDetail()),
      ),
      http.get("/api/v1/issues/:fp/transcript", () =>
        HttpResponse.json(makeTranscript()),
      ),
    );

    renderDetail();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("无法加载问题单详情");
    expect(alert).toHaveTextContent("store read failed");
    expect(alert).toHaveTextContent("internal");
    expect(within(alert).queryByRole("button")).not.toBeInTheDocument();

    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    expect(
      await screen.findByRole("heading", {
        level: 2,
        name: "Average panics on empty slice",
      }),
    ).toBeInTheDocument();
  });

  it("explains the no-run state instead of inventing evidence", async () => {
    useDetail(
      makeIssueDetail({
        issue: fixtureIssue(),
        runs: [],
        similar: { count: 0, items: [] },
        timeline: [],
      }),
    );

    renderDetail();

    expect(await screen.findAllByText("等待 runner 领取")).toHaveLength(2);
    expect(screen.getByText("暂无运行记录，等待 runner 领取。")).toBeInTheDocument();
    expect(screen.getByText("暂无时间线事件。")).toBeInTheDocument();
    expect(screen.getByText("没有同名问题单。")).toBeInTheDocument();
  });

  it("offers no owner action for an issue in a state that disallows it", async () => {
    server.use(
      mockIssueDetail({
        issue: makeIssue({
          fingerprint: "fp-detail-001",
          title: "Average panics on empty slice",
          state: "new",
          stuck: false,
          pr_url: null,
          ci_verdict: null,
        }),
      }),
      mockTranscript(),
    );

    renderDetail();

    const title = await screen.findByRole("heading", {
      level: 2,
      name: "Average panics on empty slice",
    });
    const header = title.closest("header") as HTMLElement;
    expect(within(header).queryByRole("button", { name: /^重新打开：/ })).not.toBeInTheDocument();
    expect(within(header).queryByRole("button", { name: /^重排：/ })).not.toBeInTheDocument();
  });

  it("reopens an escalated issue from the header", async () => {
    document.cookie = "nuwa_csrf=csrf-token; path=/";
    let posted: string | null = null;
    server.use(
      mockIssueDetail({ issue: fixtureIssue() }),
      mockTranscript(),
      http.post("/api/v1/issues/:fp/reopen", ({ request, params }) => {
        posted = `${String(params.fp)}:${request.headers.get("x-nuwa-csrf")}`;
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderDetail();

    const title = await screen.findByRole("heading", {
      level: 2,
      name: "Average panics on empty slice",
    });
    const header = title.closest("header") as HTMLElement;
    expect(within(header).queryByRole("button", { name: /^重排：/ })).not.toBeInTheDocument();

    fireEvent.click(within(header).getByRole("button", { name: "重新打开：fp-detail-…" }));

    await waitFor(() => expect(posted).toBe("fp-detail-001:csrf-token"));
    expect(await screen.findByText("已重新入队")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
