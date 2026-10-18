import { fireEvent, screen, waitFor, within } from "@testing-library/react";
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
    expect(
      within(table).queryByRole("link", { name: /__config__/ }),
    ).not.toBeInTheDocument();
    expect(
      within(table).queryByRole("link", { name: /__runner__/ }),
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

  it("applies the audit preset and exposes the audit badge", async () => {
    const requests = useEventsHandler(() =>
      eventsResponse([makeEvent({ fingerprint: "__config__", kind: "config_changed" })]),
    );
    renderApp("/events?page=3");

    await screen.findByRole("region", { name: "活动列表" });
    const preset = screen.getByRole("button", { name: "配置审计" });
    expect(preset).toHaveAttribute("aria-pressed", "false");
    expect(screen.queryByText("审计视图")).not.toBeInTheDocument();

    fireEvent.click(preset);

    await waitFor(() => expect(requests.length).toBeGreaterThan(1));
    const last = requests[requests.length - 1];
    expect(last.searchParams.get("kind")).toBe(
      "config_changed,config_migrated,restart_requested",
    );
    expect(last.searchParams.get("page")).toBe("1");
    expect(last.searchParams.get("from_ms")).not.toBeNull();

    expect(preset).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("审计视图")).toBeInTheDocument();
    expect(screen.getByLabelText("时间范围")).toHaveValue("30d");
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

  it("toggles kind chips", async () => {
    const requests = useEventsHandler(() => eventsResponse([makeEvent()]));
    renderApp("/events");

    await screen.findByRole("region", { name: "活动列表" });
    const chip = screen.getByRole("button", { name: "CI 失败" });
    expect(chip).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(chip);
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].searchParams.get("kind")).toBe("ci_red");
    expect(screen.getByRole("button", { name: "CI 失败" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    fireEvent.click(screen.getByRole("button", { name: "CI 失败" }));
    // Removing the chip returns to the already-cached default query, so no
    // third request is issued; the pressed state proves the URL round-trip.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "CI 失败" })).toHaveAttribute(
        "aria-pressed",
        "false",
      ),
    );
    expect(requests).toHaveLength(2);
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
