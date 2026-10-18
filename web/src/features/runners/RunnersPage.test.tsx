import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { afterEach, describe, expect, it } from "vitest";

import { resetConnectivity } from "../../lib/connectivity";
import {
  makeRunner,
  mockMe,
  mockOverviewResources,
  mockRunners,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";
import type { RunnerDto } from "./types";

afterEach(resetConnectivity);

/** The shell polls /overview, /events and /trends on every route. */
function signedIn(runners: RunnerDto[] = [makeRunner()]) {
  server.use(mockMe(), ...mockOverviewResources(), mockRunners(runners));
}

describe("RunnersPage", () => {
  it("renders the fleet summary and one card per runner", async () => {
    signedIn([
      makeRunner({
        id: "runner-a",
        status: "working",
        current_fingerprint: "fp-1",
        phase: "repair",
      }),
      makeRunner({ id: "runner-b", status: "standing_down", prs_today: 5, daily_pr_limit: 5 }),
      makeRunner({ id: "runner-c", status: "error", last_error: "provider 401", offline: true }),
    ]);

    renderApp("/runners");

    expect(await screen.findByRole("heading", { level: 2, name: "运行器" })).toBeInTheDocument();

    expect(await screen.findByTestId("runner-summary-online")).toHaveTextContent(/在线\s*2\/3/);
    expect(screen.getByTestId("runner-summary-working")).toHaveTextContent(/工作中\s*1/);
    expect(screen.getByTestId("runner-summary-idle")).toHaveTextContent(/待命\s*0/);
    expect(screen.getByTestId("runner-summary-standing-down")).toHaveTextContent(/配额待命\s*1/);
    expect(screen.getByTestId("runner-summary-error")).toHaveTextContent(/异常\s*0/);

    const cardA = await screen.findByTestId("runner-card-runner-a");
    expect(screen.getAllByTestId(/^runner-card-/)).toHaveLength(3);
    expect(within(cardA).getByText("runner-a")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /runner-a/ })).not.toBeInTheDocument();
    expect(within(cardA).getByRole("link", { name: "查看当前单：fp-1" })).toHaveAttribute(
      "href",
      "/issues/fp-1",
    );
    expect(within(cardA).getByRole("link", { name: /fp-1/ })).toHaveAttribute(
      "href",
      "/issues/fp-1",
    );
    expect(within(cardA).getByText("修复")).toBeInTheDocument();

    const cardB = screen.getByTestId("runner-card-runner-b");
    expect(within(cardB).getByText("配额待命")).toBeInTheDocument();
    expect(within(cardB).getByText("5/5")).toBeInTheDocument();
    expect(within(cardB).getByText("队列为空")).toBeInTheDocument();

    const cardC = screen.getByTestId("runner-card-runner-c");
    expect(within(cardC).getByText("离线")).toBeInTheDocument();
    expect(within(cardC).getByText("provider 401")).toBeInTheDocument();
  });

  it("treats a stale heartbeat as offline even when the API says online", async () => {
    signedIn([
      makeRunner({ id: "runner-live" }),
      makeRunner({
        id: "runner-stale",
        heartbeat_at_ms: Date.now() - 3 * 60_000,
        offline: false,
      }),
    ]);

    renderApp("/runners");

    const stale = await screen.findByTestId("runner-card-runner-stale");
    expect(within(stale).getByText("离线")).toBeInTheDocument();
    expect(screen.getByTestId("runner-summary-online")).toHaveTextContent(/在线\s*1\/2/);
  });

  it("shows only the missing-quota text when no limit is reported", async () => {
    signedIn([
      makeRunner({ id: "runner-no-quota", daily_pr_limit: null, prs_today: 3 }),
    ]);

    renderApp("/runners");

    const card = await screen.findByTestId("runner-card-runner-no-quota");
    expect(within(card).getByText("未上报配额")).toBeInTheDocument();
    expect(
      screen.queryByRole("progressbar", { name: "今日 PR 配额" }),
    ).not.toBeInTheDocument();
  });

  it("shows a start command when no runner has registered", async () => {
    signedIn([]);

    renderApp("/runners");

    expect(await screen.findByText("还没有运行器注册")).toBeInTheDocument();
    expect(screen.getByText(/nuwa work --loop --runner-id/)).toBeInTheDocument();
    expect(screen.queryByTestId(/^runner-card-/)).not.toBeInTheDocument();
  });

  it("keeps the stale fleet visible and retries after a failed refetch", async () => {
    let calls = 0;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      http.get("/api/v1/runners", () => {
        calls += 1;
        if (calls === 2) {
          return HttpResponse.json(
            { error: { code: "internal_error", message: "store read failed" } },
            { status: 500 },
          );
        }
        return HttpResponse.json({ data: [makeRunner({ id: "runner-after-stale" })] });
      }),
    );

    const { queryClient } = renderApp("/runners");
    await screen.findByTestId("runner-card-runner-after-stale");

    await act(async () => {
      await queryClient
        .refetchQueries({ queryKey: ["runners"] })
        .catch(() => undefined);
    });

    expect(
      await screen.findByText("运行器状态更新失败，显示的是上次数据。"),
    ).toBeInTheDocument();
    // The cached cards stay visible below the stale notice.
    expect(screen.getByTestId("runner-card-runner-after-stale")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => expect(calls).toBe(3));
    await waitFor(() =>
      expect(
        screen.queryByText("运行器状态更新失败，显示的是上次数据。"),
      ).not.toBeInTheDocument(),
    );
  });

  it("retries a failed fleet request", async () => {
    server.use(mockMe(), ...mockOverviewResources());
    server.use(
      http.get("/api/v1/runners", () =>
        HttpResponse.json(
          { error: { code: "internal_error", message: "store read failed" } },
          { status: 500 },
        ),
      ),
    );

    renderApp("/runners");

    expect(await screen.findByText("无法加载运行器")).toBeInTheDocument();

    server.use(mockRunners([makeRunner({ id: "runner-after-retry" })]));
    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    expect(await screen.findByTestId("runner-card-runner-after-retry")).toBeInTheDocument();
  });
});
