import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { delay, http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import { mockMe, mockOverview, server } from "../../test/msw";
import { renderApp } from "../../test/render";
import { formatDateTime, shortFingerprint } from "../overview/format";
import type { EventItem, EventsResponse, PageMeta } from "./types";

const NOW = Date.now();

function makeEvent(overrides: Partial<EventItem> = {}): EventItem {
  const ts = NOW - 60_000;
  return {
    fingerprint: "fp-default",
    kind: "reported",
    detail: "panic: index out of range",
    ts_ms: ts,
    repo: "example.com/app",
    actor: "runner",
    count: 1,
    first_ts_ms: ts,
    last_ts_ms: ts,
    ...overrides,
  };
}

function eventsResponse(
  data: EventItem[],
  meta: Partial<PageMeta> = {},
): EventsResponse {
  return {
    data,
    meta: { page: 1, page_size: 50, total: data.length, ...meta },
  };
}

/**
 * Registers the shell dependencies and a dynamic `/events` handler that
 * records every request, mirroring the issues-page test harness.
 */
function useEventsHandler(respond: (url: URL) => EventsResponse): URL[] {
  const requests: URL[] = [];
  server.use(
    mockMe(),
    mockOverview(),
    http.get("/api/v1/events", ({ request }) => {
      const url = new URL(request.url);
      requests.push(url);
      return HttpResponse.json(respond(url));
    }),
  );
  return requests;
}

describe("EventsPage", () => {
  it("renders system labels without links and links issue fingerprints", async () => {
    const issueFingerprint = "abcdef1234567890";
    const requests = useEventsHandler(() =>
      eventsResponse([
        makeEvent({
          fingerprint: "__config__",
          kind: "config_changed",
          detail: "NUWA_INTERVAL",
          actor: "owner",
          repo: "",
        }),
        makeEvent({
          fingerprint: "__runner__:worker-1",
          kind: "restart_requested",
          detail: "restart",
          actor: "system",
        }),
        makeEvent({
          fingerprint: "__system__",
          kind: "sync",
          detail: "reconcile queue",
          actor: "system",
        }),
        makeEvent({
          fingerprint: issueFingerprint,
          kind: "state_changed",
          detail: "new -> triggered",
          ts_ms: NOW - 120_000,
        }),
      ]),
    );

    renderApp("/events");

    expect(await screen.findByRole("heading", { level: 2, name: "活动" })).toBeInTheDocument();
    const table = await screen.findByRole("region", { name: "活动列表" });

    expect(within(table).getByText("系统 · 配置")).toBeInTheDocument();
    expect(within(table).getByText("系统 · 运行器")).toBeInTheDocument();
    expect(within(table).getByTitle("__system__")).toHaveTextContent("系统");
    // The visible label drops the id, so the full fingerprint is announced via
    // an sr-only suffix (same pattern as the overview's recent events).
    expect(within(table).getByText("（__config__）")).toBeInTheDocument();
    expect(within(table).getByText("（__runner__:worker-1）")).toBeInTheDocument();
    expect(within(table).getByText("（__system__）")).toBeInTheDocument();
    expect(
      within(table).queryByRole("link", { name: /__config__/ }),
    ).not.toBeInTheDocument();
    expect(
      within(table).queryByRole("link", { name: /__runner__/ }),
    ).not.toBeInTheDocument();
    expect(
      within(table).queryByRole("link", { name: "__system__" }),
    ).not.toBeInTheDocument();

    const issueLink = within(table).getByRole("link", {
      name: shortFingerprint(issueFingerprint),
    });
    expect(issueLink).toHaveAttribute("href", `/issues/${issueFingerprint}`);

    // The mobile card list carries the same labels but no system links either.
    const cards = screen.getByRole("list", { name: "活动卡片列表" });
    expect(within(cards).getByText("系统 · 配置")).toBeInTheDocument();
    expect(
      within(cards).getByRole("link", { name: shortFingerprint(issueFingerprint) }),
    ).toHaveAttribute("href", `/issues/${issueFingerprint}`);

    expect(requests).toHaveLength(1);
    expect(requests[0].searchParams.get("page_size")).toBe("50");
    expect(requests[0].searchParams.get("from_ms")).not.toBeNull();
  });

  it("applies the audit preset as an action and exposes the audit badge", async () => {
    const requests = useEventsHandler(() =>
      eventsResponse([makeEvent({ fingerprint: "__config__", kind: "config_changed" })]),
    );
    renderApp("/events?page=3");

    await screen.findByRole("region", { name: "活动列表" });
    const preset = screen.getByRole("button", { name: "配置审计" });
    // A preset is an action, not a toggle: it never advertises a pressed state.
    expect(preset).not.toHaveAttribute("aria-pressed");
    expect(screen.queryByText("审计视图")).not.toBeInTheDocument();

    fireEvent.click(preset);

    await waitFor(() => expect(requests.length).toBeGreaterThan(1));
    const last = requests[requests.length - 1];
    expect(last.searchParams.get("kind")).toBe(
      "config_changed,config_migrated,restart_requested",
    );
    expect(last.searchParams.get("page")).toBe("1");
    expect(last.searchParams.get("from_ms")).not.toBeNull();

    expect(preset).not.toHaveAttribute("aria-pressed");
    expect(screen.getByText("审计视图")).toBeInTheDocument();
    expect(screen.getByLabelText("时间范围")).toHaveValue("30d");

    // The badge is driven by the exact filter state: dropping one audit kind
    // leaves the 30-day window intact but the page is no longer the audit view.
    fireEvent.click(screen.getByRole("button", { name: "配置变更" }));
    await waitFor(() => expect(screen.queryByText("审计视图")).not.toBeInTheDocument());
    expect(screen.getByLabelText("时间范围")).toHaveValue("30d");
    expect(requests[requests.length - 1].searchParams.get("kind")).toBe(
      "config_migrated,restart_requested",
    );
  });

  it("commits text filters on Enter/blur, resets the page and aggregates", async () => {
    const requests = useEventsHandler((url) =>
      eventsResponse([makeEvent()], {
        page: Number(url.searchParams.get("page") ?? "1"),
        total: 120,
      }),
    );
    renderApp("/events?page=2");

    await screen.findByRole("region", { name: "活动列表" });
    expect(requests[0].searchParams.get("page")).toBe("2");

    const repo = screen.getByLabelText("仓库");
    fireEvent.change(repo, { target: { value: "example.com/app" } });
    fireEvent.blur(repo);
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("repo")).toBe("example.com/app");
    expect(requests[1].searchParams.get("page")).toBe("1");

    const fingerprint = screen.getByLabelText("指纹");
    fireEvent.change(fingerprint, { target: { value: "fp-1" } });
    fireEvent.keyDown(fingerprint, { key: "Enter" });
    await waitFor(() => expect(requests).toHaveLength(3));
    expect(requests[2].searchParams.get("fingerprint")).toBe("fp-1");
    expect(requests[2].searchParams.get("repo")).toBe("example.com/app");

    const aggregate = screen.getByRole("checkbox", {
      name: "聚合历史上报（按小时）",
    });
    fireEvent.click(aggregate);
    await waitFor(() => expect(requests).toHaveLength(4));
    expect(requests[3].searchParams.get("aggregate")).toBe("1");

    fireEvent.change(screen.getByLabelText("时间范围"), { target: { value: "7d" } });
    await waitFor(() => expect(requests).toHaveLength(5));
    expect(requests[4].searchParams.get("aggregate")).toBe("1");
    expect(requests[4].searchParams.get("from_ms")).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "清除筛选" }));
    await waitFor(() => expect(requests).toHaveLength(6));
    expect(requests[5].searchParams.get("repo")).toBeNull();
    expect(requests[5].searchParams.get("fingerprint")).toBeNull();
    expect(requests[5].searchParams.get("aggregate")).toBeNull();
    expect(requests[5].searchParams.get("page")).toBe("1");
    expect(screen.getByLabelText("仓库")).toHaveValue("");
  });

  it("restores kind and range filters from the URL", async () => {
    const requests = useEventsHandler(() => eventsResponse([makeEvent({ kind: "ci_red" })]));
    renderApp("/events?kind=ci_red&range=7d");

    await screen.findByRole("region", { name: "活动列表" });
    expect(screen.getByRole("button", { name: "CI 失败" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByRole("button", { name: "上报" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );

    const range = screen.getByLabelText("时间范围") as HTMLSelectElement;
    expect(range).toHaveValue("7d");
    expect(range.selectedOptions[0]).toHaveTextContent("最近 7 天");

    expect(requests[0].searchParams.get("kind")).toBe("ci_red");
    const fromMs = Number(requests[0].searchParams.get("from_ms"));
    expect(Date.now() - fromMs).toBeGreaterThan(6 * 86_400_000);
    expect(Date.now() - fromMs).toBeLessThan(8 * 86_400_000);
  });

  it("restores the aggregate flag from the URL", async () => {
    useEventsHandler(() => eventsResponse([makeEvent()]));
    renderApp("/events?aggregate=1");

    await screen.findByRole("region", { name: "活动列表" });
    expect(
      screen.getByRole("checkbox", { name: "聚合历史上报（按小时）" }),
    ).toBeChecked();
  });

  it("toggles kind chips", async () => {
    const requests = useEventsHandler(() => eventsResponse([makeEvent()]));
    renderApp("/events");

    await screen.findByRole("region", { name: "活动列表" });
    const lastKind = () => requests[requests.length - 1].searchParams.get("kind");
    expect(screen.getByRole("button", { name: "CI 失败" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );

    fireEvent.click(screen.getByRole("button", { name: "CI 失败" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "CI 失败" })).toHaveAttribute(
        "aria-pressed",
        "true",
      ),
    );
    expect(lastKind()).toBe("ci_red");

    // Selecting a second chip keeps the canonical order.
    fireEvent.click(screen.getByRole("button", { name: "上报" }));
    await waitFor(() => expect(lastKind()).toBe("reported,ci_red"));

    // Untoggling a chip removes only that kind; anchor on the pressed state and
    // the resulting request URL instead of a total request count, which used to
    // depend on whether the default query happened to be cached.
    fireEvent.click(screen.getByRole("button", { name: "CI 失败" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "CI 失败" })).toHaveAttribute(
        "aria-pressed",
        "false",
      ),
    );
    await waitFor(() => expect(lastKind()).toBe("reported"));
  });

  it("keeps cached rows and shows a stale notice when a refetch fails", async () => {
    const fingerprint = "abcdef1234567890";
    let calls = 0;
    server.use(
      mockMe(),
      mockOverview(),
      http.get("/api/v1/events", () => {
        calls += 1;
        if (calls > 1) {
          return HttpResponse.json(
            { error: { code: "internal", message: "store read failed" } },
            { status: 500 },
          );
        }
        return HttpResponse.json(eventsResponse([makeEvent({ fingerprint })]));
      }),
    );
    const { queryClient } = renderApp("/events");

    const table = await screen.findByRole("region", { name: "活动列表" });
    await act(async () => {
      await queryClient.refetchQueries().catch(() => undefined);
    });

    expect(
      await screen.findByText("活动更新失败，显示的是上次数据。"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    // The cached rows stay visible below the stale notice.
    expect(
      within(table).getByRole("link", { name: shortFingerprint(fingerprint) }),
    ).toBeInTheDocument();
  });

  it("shows the aggregated count and the hour range", async () => {
    const first = NOW - 3_600_000;
    const last = NOW - 60_000;
    useEventsHandler(() =>
      eventsResponse([
        makeEvent({
          fingerprint: "abcdef1234567890",
          kind: "reported",
          count: 7,
          first_ts_ms: first,
          last_ts_ms: last,
          ts_ms: last,
        }),
      ]),
    );
    renderApp("/events?aggregate=1");

    const table = await screen.findByRole("region", { name: "活动列表" });
    expect(within(table).getByText("×7")).toBeInTheDocument();
    expect(
      within(table).getByText(`${formatDateTime(first)} – ${formatDateTime(last)}`),
    ).toBeInTheDocument();
  });

  it("paginates with next/prev using 50 rows per page", async () => {
    const requests = useEventsHandler((url) =>
      eventsResponse([makeEvent()], {
        page: Number(url.searchParams.get("page") ?? "1"),
        total: 120,
      }),
    );
    renderApp("/events");

    const pagination = await screen.findByRole("navigation", { name: "活动分页" });
    expect(pagination).toHaveTextContent("第 1 / 3 页");
    expect(
      within(pagination).getByRole("button", { name: "上一页" }),
    ).toBeDisabled();

    fireEvent.click(within(pagination).getByRole("button", { name: "下一页" }));
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("page")).toBe("2");
    expect(requests[1].searchParams.get("page_size")).toBe("50");
    await waitFor(() =>
      expect(screen.getByRole("navigation", { name: "活动分页" })).toHaveTextContent(
        "第 2 / 3 页",
      ),
    );
  });

  it("recovers from an out-of-range page", async () => {
    const requests = useEventsHandler((url) =>
      eventsResponse([], {
        page: Number(url.searchParams.get("page") ?? "1"),
        total: 60,
      }),
    );
    renderApp("/events?page=2");

    expect(await screen.findByText("该页没有事件")).toBeInTheDocument();
    expect(screen.getByText(/共 60 条事件，当前页超出范围/)).toBeInTheDocument();
    expect(requests[0].searchParams.get("page")).toBe("2");
    expect(requests[0].searchParams.get("page_size")).toBe("50");

    fireEvent.click(screen.getByRole("button", { name: "回到第一页" }));

    await waitFor(() =>
      expect(requests[requests.length - 1].searchParams.get("page")).toBe("1"),
    );
    expect(await screen.findByText("这个时间窗内没有事件")).toBeInTheDocument();
  });

  it("shows the empty state for an eventless window", async () => {
    useEventsHandler(() => eventsResponse([]));
    renderApp("/events");

    expect(await screen.findByText("这个时间窗内没有事件")).toBeInTheDocument();
    expect(
      screen.getByText("调整类型或时间范围，或等待新的上报/运行记录。"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "活动列表" })).not.toBeInTheDocument();
  });

  it("shows a retryable error when the feed fails", async () => {
    let fail = true;
    server.use(
      mockMe(),
      mockOverview(),
      http.get("/api/v1/events", () =>
        fail
          ? HttpResponse.json(
              { error: { code: "internal", message: "store read failed" } },
              { status: 500 },
            )
          : HttpResponse.json(eventsResponse([makeEvent()])),
      ),
    );
    renderApp("/events");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("无法加载活动");
    expect(alert).toHaveTextContent("store read failed");
    expect(alert).toHaveTextContent("(internal)");

    fail = false;
    fireEvent.click(within(alert).getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("region", { name: "活动列表" })).toBeInTheDocument();
  });

  it("shows a loading skeleton while the feed is pending", async () => {
    server.use(
      mockMe(),
      mockOverview(),
      http.get("/api/v1/events", async () => {
        await delay("infinite");
        return HttpResponse.json(eventsResponse([]));
      }),
    );
    renderApp("/events");

    expect(await screen.findByText("正在加载活动")).toBeInTheDocument();
  });
});
