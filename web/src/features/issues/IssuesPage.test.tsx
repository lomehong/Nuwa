import { QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { delay, http, HttpResponse } from "msw";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import { ToastProvider } from "../../components/ui";
import { makeIssue, makeIssues, server } from "../../test/msw";
import { createTestQueryClient } from "../../test/render";
import { shortFingerprint } from "./format";
import { IssuesPage } from "./IssuesPage";
import type { IssueListItem, IssueListResponse } from "./types";

const NOW = Date.now();

function escalatedIssue(): IssueListItem {
  return makeIssue({
    fingerprint: "fp-escalated-001",
    title: "Average panics on empty slice",
    language: "go",
    repo: "example.com/app",
    severity: "critical",
    state: "escalated",
    occurrences: 17,
    pr_url: "https://github.com/example/app/pull/7",
    ci_verdict: "success",
    owner_runner_id: "runner-alpha",
    stuck: true,
    last_seen_ms: NOW - 5 * 60_000,
  });
}

function fixingIssue(): IssueListItem {
  return makeIssue({
    fingerprint: "fp-fixing-002",
    title: "Worker retries forever",
    language: "rust",
    repo: "example.com/worker",
    severity: "high",
    state: "fixing",
    occurrences: 4,
    last_seen_ms: NOW - 2 * 60 * 60_000,
  });
}

function pageResponse(url: URL): IssueListResponse {
  const page = Number(url.searchParams.get("page") ?? "1");
  const pageSize = Number(url.searchParams.get("page_size") ?? "50");
  return makeIssues({
    data: [escalatedIssue(), fixingIssue()],
    meta: { page, page_size: pageSize, total: 2 },
  });
}

/** Registers an issues handler that records every requested URL. */
function useIssuesHandler(respond: (url: URL) => IssueListResponse): URL[] {
  const requests: URL[] = [];
  server.use(
    http.get("/api/v1/issues", ({ request }) => {
      const url = new URL(request.url);
      requests.push(url);
      return HttpResponse.json(respond(url));
    }),
  );
  return requests;
}

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location">{`${location.pathname}${location.search}`}</div>;
}

function renderIssues(initialEntry = "/issues") {
  const queryClient = createTestQueryClient();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <MemoryRouter initialEntries={[initialEntry]}>
          <Routes>
            <Route
              path="/issues"
              element={
                <>
                  <IssuesPage />
                  <LocationProbe />
                </>
              }
            />
            <Route path="/issues/:fp" element={<h2>问题单详情占位</h2>} />
          </Routes>
        </MemoryRouter>
      </ToastProvider>
    </QueryClientProvider>,
  );
  return { ...result, queryClient };
}

describe("IssuesPage", () => {
  it("renders the table, mobile cards and pagination for the first page", async () => {
    const requests = useIssuesHandler(pageResponse);
    const { container } = renderIssues();

    const table = await screen.findByRole("region", { name: "问题单列表" });
    expect(
      within(table).getByRole("link", { name: "Average panics on empty slice" }),
    ).toHaveAttribute("href", "/issues/fp-escalated-001");
    expect(
      within(table).getByText(shortFingerprint("fp-escalated-001")),
    ).toBeInTheDocument();
    expect(within(table).getByText("严重")).toBeInTheDocument();
    expect(within(table).getByText("已升级")).toBeInTheDocument();
    expect(within(table).getByText("卡单")).toBeInTheDocument();
    expect(within(table).getByText("example.com/app")).toBeInTheDocument();
    expect(within(table).getByText("17")).toBeInTheDocument();
    expect(within(table).getByText(/分钟前/)).toBeInTheDocument();
    expect(
      within(table).getByRole("link", {
        name: "打开 PR：Average panics on empty slice",
      }),
    ).toHaveAttribute("href", "https://github.com/example/app/pull/7");
    expect(within(table).getByText("CI 通过")).toBeInTheDocument();
    expect(within(table).getByText("runner-alpha")).toBeInTheDocument();
    expect(within(table).getByText("未归属")).toBeInTheDocument();

    const cards = screen.getByRole("list", { name: "问题单卡片列表" });
    expect(
      within(cards).getByRole("link", { name: "Average panics on empty slice" }),
    ).toHaveAttribute("href", "/issues/fp-escalated-001");
    expect(
      within(cards).getByRole("button", { name: "复制指纹：fp-escalated-001" }),
    ).toBeInTheDocument();

    const pagination = screen.getByRole("navigation", { name: "问题单分页" });
    expect(pagination).toHaveTextContent("共 2 条");
    expect(pagination).toHaveTextContent("第 1 / 1 页");
    expect(within(pagination).getByRole("button", { name: "上一页" })).toBeDisabled();
    expect(within(pagination).getByRole("button", { name: "下一页" })).toBeDisabled();

    expect(requests).toHaveLength(1);
    expect(container.querySelector("[style]")).toBeNull();
  });

  it("writes filter changes to the URL and the request, resetting the page", async () => {
    const requests = useIssuesHandler(pageResponse);
    renderIssues("/issues?page=2");

    await screen.findByRole("region", { name: "问题单列表" });
    expect(requests[0].searchParams.get("page")).toBe("2");

    fireEvent.click(screen.getByRole("button", { name: "已升级" }));
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("state")).toBe("escalated");
    expect(requests[1].searchParams.get("page")).toBeNull();
    expect(screen.getByTestId("location")).toHaveTextContent("/issues?state=escalated");

    fireEvent.change(screen.getByLabelText("严重度"), { target: { value: "high" } });
    await waitFor(() => expect(requests).toHaveLength(3));
    expect(requests[2].searchParams.get("state")).toBe("escalated");
    expect(requests[2].searchParams.get("severity")).toBe("high");

    fireEvent.click(screen.getByRole("button", { name: "仅需处理" }));
    await waitFor(() => expect(requests).toHaveLength(4));
    expect(requests[3].searchParams.get("attention")).toBe("1");
    expect(screen.getByRole("button", { name: "仅需处理" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    fireEvent.click(screen.getByRole("button", { name: "仅卡单" }));
    await waitFor(() => expect(requests).toHaveLength(5));
    expect(requests[4].searchParams.get("stuck")).toBe("1");
    expect(screen.getByRole("button", { name: "仅卡单" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    fireEvent.click(screen.getByRole("button", { name: "清除筛选" }));
    await waitFor(() => expect(requests).toHaveLength(6));
    expect(requests[5].searchParams.get("state")).toBeNull();
    expect(requests[5].searchParams.get("severity")).toBeNull();
    expect(requests[5].searchParams.get("attention")).toBeNull();
    expect(requests[5].searchParams.get("stuck")).toBeNull();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/issues$/);
  });

  it("debounces the search field and only requests the final value", async () => {
    const requests = useIssuesHandler(pageResponse);
    renderIssues();

    await screen.findByRole("region", { name: "问题单列表" });
    const search = screen.getByLabelText("搜索");

    fireEvent.change(search, { target: { value: "pan" } });
    fireEvent.change(search, { target: { value: "panic" } });
    expect(requests).toHaveLength(1);

    await waitFor(() => expect(requests).toHaveLength(2), { timeout: 2_000 });
    expect(requests[1].searchParams.get("q")).toBe("panic");
    expect(screen.getByTestId("location")).toHaveTextContent("q=panic");
  });

  it("sorts from the table header and the filter selects", async () => {
    const requests = useIssuesHandler(pageResponse);
    renderIssues();

    const table = await screen.findByRole("region", { name: "问题单列表" });
    expect(within(table).getByRole("columnheader", { name: "状态" })).toHaveAttribute(
      "aria-sort",
      "none",
    );

    fireEvent.click(within(table).getByRole("button", { name: "按状态排序" }));
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("sort")).toBe("state");
    expect(requests[1].searchParams.get("order")).toBeNull();
    await waitFor(() =>
      expect(within(table).getByRole("columnheader", { name: "状态" })).toHaveAttribute(
        "aria-sort",
        "descending",
      ),
    );

    fireEvent.click(
      within(table).getByRole("button", { name: "按状态排序，当前降序" }),
    );
    await waitFor(() => expect(requests).toHaveLength(3));
    expect(requests[2].searchParams.get("sort")).toBe("state");
    expect(requests[2].searchParams.get("order")).toBe("asc");
    expect(within(table).getByRole("columnheader", { name: "状态" })).toHaveAttribute(
      "aria-sort",
      "ascending",
    );

    fireEvent.change(screen.getByLabelText("排序"), {
      target: { value: "occurrences" },
    });
    await waitFor(() => expect(requests).toHaveLength(4));
    expect(requests[3].searchParams.get("sort")).toBe("occurrences");
    expect(requests[3].searchParams.get("order")).toBe("asc");

    fireEvent.change(screen.getByLabelText("排序方向"), {
      target: { value: "desc" },
    });
    await waitFor(() => expect(requests).toHaveLength(5));
    expect(requests[4].searchParams.get("order")).toBeNull();
  });

  it("paginates with next/prev and resets to page 1 when page size changes", async () => {
    const requests = useIssuesHandler((url) => {
      const page = Number(url.searchParams.get("page") ?? "1");
      const pageSize = Number(url.searchParams.get("page_size") ?? "50");
      return makeIssues({
        data: [escalatedIssue()],
        meta: { page, page_size: pageSize, total: 120 },
      });
    });
    renderIssues();

    const pagination = await screen.findByRole("navigation", { name: "问题单分页" });
    expect(pagination).toHaveTextContent("第 1 / 3 页");
    expect(within(pagination).getByRole("button", { name: "上一页" })).toBeDisabled();

    fireEvent.click(within(pagination).getByRole("button", { name: "下一页" }));
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("page")).toBe("2");
    await waitFor(() =>
      expect(screen.getByRole("navigation", { name: "问题单分页" })).toHaveTextContent(
        "第 2 / 3 页",
      ),
    );

    fireEvent.click(
      within(screen.getByRole("navigation", { name: "问题单分页" })).getByRole("button", {
        name: "上一页",
      }),
    );
    // Page 1 is still fresh in the query cache (<15s), so going back paints
    // the cached page without a second network round-trip.
    await waitFor(() =>
      expect(screen.getByRole("navigation", { name: "问题单分页" })).toHaveTextContent(
        "第 1 / 3 页",
      ),
    );
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/issues$/);
    expect(requests).toHaveLength(2);

    fireEvent.change(screen.getByLabelText("每页条数"), { target: { value: "100" } });
    await waitFor(() => expect(requests).toHaveLength(3));
    expect(requests[2].searchParams.get("page_size")).toBe("100");
    expect(requests[2].searchParams.get("page")).toBeNull();
    await waitFor(() =>
      expect(screen.getByRole("navigation", { name: "问题单分页" })).toHaveTextContent(
        "第 1 / 2 页",
      ),
    );
  });

  it("shows the no-data empty state", async () => {
    useIssuesHandler(() =>
      makeIssues({ data: [], meta: { page: 1, page_size: 50, total: 0 } }),
    );
    renderIssues();

    expect(await screen.findByText("暂无问题单")).toBeInTheDocument();
    expect(screen.getByText("运行器上报信号后，问题单会出现在这里。")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "问题单列表" })).not.toBeInTheDocument();
  });

  it("distinguishes filtered emptiness and clears the filters", async () => {
    const requests = useIssuesHandler((url) =>
      makeIssues({
        data: [],
        meta: { page: Number(url.searchParams.get("page") ?? "1"), page_size: 50, total: 0 },
      }),
    );
    renderIssues("/issues?q=panic");

    expect(await screen.findByText("筛选无结果")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "清除筛选条件" }));

    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("q")).toBeNull();
    expect(await screen.findByText("暂无问题单")).toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/issues$/);
  });

  it("shows a retryable error when the list fails", async () => {
    let fail = true;
    server.use(
      http.get("/api/v1/issues", () =>
        fail
          ? HttpResponse.json(
              { error: { code: "internal", message: "store read failed" } },
              { status: 500 },
            )
          : HttpResponse.json(pageResponse(new URL("http://localhost/api/v1/issues"))),
      ),
    );
    renderIssues();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("无法加载问题单");
    expect(alert).toHaveTextContent("store read failed");
    expect(alert).toHaveTextContent("internal");

    fail = false;
    fireEvent.click(within(alert).getByRole("button", { name: "重试" }));
    expect(
      await screen.findByRole("region", { name: "问题单列表" }),
    ).toBeInTheDocument();
  });

  it("explains a 422 validation failure with the offending fields", async () => {
    server.use(
      http.get("/api/v1/issues", () =>
        HttpResponse.json(
          {
            error: {
              code: "validation_failed",
              message: "validation failed",
              fields: { page_size: "must be between 1 and 200" },
            },
          },
          { status: 422 },
        ),
      ),
    );
    renderIssues("/issues?page_size=100");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("筛选条件无效");
    expect(alert).toHaveTextContent("page_size：must be between 1 and 200");
    expect(alert).toHaveTextContent("validation_failed");
  });

  it("caps a 129-character search commit and surfaces the 422 field", async () => {
    const longQ = "x".repeat(129);
    const requests: URL[] = [];
    server.use(
      http.get("/api/v1/issues", ({ request }) => {
        const url = new URL(request.url);
        requests.push(url);
        if (url.searchParams.get("q")) {
          // Forced 422 so the error path is exercised even though the client
          // truncates the term before sending it.
          return HttpResponse.json(
            {
              error: {
                code: "validation_failed",
                message: "validation failed",
                fields: { q: "must be at most 128 characters" },
              },
            },
            { status: 422 },
          );
        }
        return HttpResponse.json(pageResponse(url));
      }),
    );
    renderIssues();

    await screen.findByRole("region", { name: "问题单列表" });
    const search = screen.getByLabelText("搜索");
    expect(search).toHaveAttribute("maxlength", "128");

    fireEvent.change(search, { target: { value: longQ } });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("筛选条件无效");
    expect(alert).toHaveTextContent("q：must be at most 128 characters");
    expect(alert).toHaveTextContent("validation_failed");

    // The 129-character commit is capped before it reaches the wire.
    expect(requests).toHaveLength(2);
    expect(requests[1].searchParams.get("q")).toBe("x".repeat(128));
  });

  it("surfaces a 422 above cached rows when a background refetch fails", async () => {
    let calls = 0;
    server.use(
      http.get("/api/v1/issues", ({ request }) => {
        calls += 1;
        if (calls > 1) {
          return HttpResponse.json(
            {
              error: {
                code: "validation_failed",
                message: "validation failed",
                fields: { q: "must be at most 128 characters" },
              },
            },
            { status: 422 },
          );
        }
        return HttpResponse.json(pageResponse(new URL(request.url)));
      }),
    );
    const { queryClient } = renderIssues("/issues?q=panic");

    const table = await screen.findByRole("region", { name: "问题单列表" });
    await act(async () => {
      await queryClient.refetchQueries().catch(() => undefined);
    });

    const banner = await screen.findByText("问题单更新失败，正在显示上次数据。");
    // The toast provider also mounts an empty live region, so anchor on the
    // stale banner text and walk up to its status container.
    const status = banner.closest<HTMLElement>('[role="status"]');
    if (!status) {
      throw new Error("stale-data banner is not inside a status region");
    }
    expect(status).toHaveTextContent("筛选条件无效");
    expect(status).toHaveTextContent("q：must be at most 128 characters");
    expect(status).toHaveTextContent("validation_failed");
    // The retry button lives outside the live region.
    expect(within(status).queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();

    // The cached rows stay visible below the banner.
    expect(
      within(table).getByRole("link", { name: "Average panics on empty slice" }),
    ).toBeInTheDocument();
  });

  it("clears a pending debounced draft when filters are cleared", async () => {
    const requests = useIssuesHandler(pageResponse);
    renderIssues("/issues?state=escalated");

    await screen.findByRole("region", { name: "问题单列表" });
    const search = screen.getByLabelText("搜索");
    // The draft is still inside its debounce window when clear is pressed.
    fireEvent.change(search, { target: { value: "panic" } });
    fireEvent.click(screen.getByRole("button", { name: "清除筛选" }));

    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("q")).toBeNull();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/issues$/);
    expect(search).toHaveValue("");

    // Wait past the debounce window: the abandoned draft must not commit.
    await new Promise((resolve) => window.setTimeout(resolve, 450));
    expect(requests).toHaveLength(2);
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/issues$/);
  });

  it("shows the URL page in the footer even when meta trails behind", async () => {
    useIssuesHandler(() =>
      makeIssues({
        data: [escalatedIssue()],
        // Simulates `keepPreviousData` painting page 1 while page 2 loads.
        meta: { page: 1, page_size: 50, total: 120 },
      }),
    );
    renderIssues("/issues?page=2");

    const pagination = await screen.findByRole("navigation", { name: "问题单分页" });
    expect(pagination).toHaveTextContent("第 2 / 3 页");
  });

  it("links rows to the detail page and copies the fingerprint with a toast", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    useIssuesHandler(pageResponse);
    renderIssues();

    const table = await screen.findByRole("region", { name: "问题单列表" });
    const titleLink = within(table).getByRole("link", {
      name: "Average panics on empty slice",
    });
    expect(titleLink).toHaveAttribute("href", "/issues/fp-escalated-001");
    expect(
      within(table).getByRole("link", {
        name: "查看详情：Average panics on empty slice",
      }),
    ).toHaveAttribute("href", "/issues/fp-escalated-001");

    fireEvent.click(
      within(table).getByRole("button", { name: "复制指纹：fp-escalated-001" }),
    );
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("fp-escalated-001"));
    expect(await screen.findByText("已复制指纹")).toBeInTheDocument();

    // The short-fingerprint affordance copies the full value and says so.
    fireEvent.click(
      within(table).getByRole("button", {
        name: "复制指纹（完整）：fp-escalated-001",
      }),
    );
    await waitFor(() => expect(writeText).toHaveBeenCalledTimes(2));
    expect(writeText).toHaveBeenLastCalledWith("fp-escalated-001");

    fireEvent.click(titleLink);
    expect(
      await screen.findByRole("heading", { name: "问题单详情占位" }),
    ).toBeInTheDocument();
  });

  it("offers the owner action only for states that allow it", async () => {
    document.cookie = "nuwa_csrf=csrf-token; path=/";
    let posted: string | null = null;
    server.use(
      http.get("/api/v1/issues", ({ request }) =>
        HttpResponse.json(pageResponse(new URL(request.url))),
      ),
      http.post("/api/v1/issues/:fp/reopen", ({ request, params }) => {
        posted = `${String(params.fp)}:${request.headers.get("x-nuwa-csrf")}`;
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderIssues();

    const table = await screen.findByRole("region", { name: "问题单列表" });
    // The escalated row can reopen; the healthy fixing row has no action.
    expect(
      within(table).getAllByRole("button", { name: "重新打开：fp-escalat…" }),
    ).toHaveLength(1);
    expect(within(table).queryByRole("button", { name: /^重排：/ })).not.toBeInTheDocument();

    const cards = screen.getByRole("list", { name: "问题单卡片列表" });
    expect(
      within(cards).getAllByRole("button", { name: "重新打开：fp-escalat…" }),
    ).toHaveLength(1);

    fireEvent.click(within(table).getByRole("button", { name: "重新打开：fp-escalat…" }));

    await waitFor(() => expect(posted).toBe("fp-escalated-001:csrf-token"));
    expect(await screen.findByText("已重新入队")).toBeInTheDocument();
  });

  it("shows a loading skeleton while the first page is pending", async () => {
    server.use(
      http.get("/api/v1/issues", async () => {
        await delay("infinite");
        return HttpResponse.json(makeIssues());
      }),
    );
    renderIssues();

    expect(await screen.findByText("正在加载问题单")).toBeInTheDocument();
  });

  it("round-trips filters, sort and pagination from the initial URL", async () => {
    const requests = useIssuesHandler((url) => {
      const page = Number(url.searchParams.get("page") ?? "1");
      const pageSize = Number(url.searchParams.get("page_size") ?? "50");
      return makeIssues({
        data: [escalatedIssue()],
        meta: { page, page_size: pageSize, total: 120 },
      });
    });
    renderIssues(
      "/issues?state=fixing,new&severity=high&repo=example.com%2Fapp&language=go" +
        "&q=panic%3A%20boom&stuck=1&attention=1&sort=occurrences&order=asc" +
        "&page=2&page_size=100",
    );

    await screen.findByRole("region", { name: "问题单列表" });
    expect(Object.fromEntries(requests[0].searchParams)).toEqual({
      state: "new,fixing",
      severity: "high",
      repo: "example.com/app",
      language: "go",
      q: "panic: boom",
      stuck: "1",
      attention: "1",
      sort: "occurrences",
      order: "asc",
      page: "2",
      page_size: "100",
    });

    expect(screen.getByLabelText("搜索")).toHaveValue("panic: boom");
    expect(screen.getByLabelText("仓库")).toHaveValue("example.com/app");
    expect(screen.getByLabelText("语言")).toHaveValue("go");
    expect(screen.getByLabelText("严重度")).toHaveValue("high");
    expect(screen.getByLabelText("排序")).toHaveValue("occurrences");
    expect(screen.getByLabelText("排序方向")).toHaveValue("asc");
    expect(screen.getByLabelText("每页条数")).toHaveValue("100");

    expect(screen.getByRole("button", { name: "新建" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByRole("button", { name: "修复中" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByRole("button", { name: "已升级" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
    expect(screen.getByRole("button", { name: "仅需处理" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByRole("button", { name: "仅卡单" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    expect(screen.getByRole("navigation", { name: "问题单分页" })).toHaveTextContent(
      "第 2 / 2 页",
    );
  });
});
