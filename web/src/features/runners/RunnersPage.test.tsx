import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { afterEach, describe, expect, it } from "vitest";

import { resetConnectivity } from "../../lib/connectivity";
import {
  makeProcessConfiguration,
  makeRunner,
  makeStartupConfig,
  mockMe,
  mockOverviewResources,
  mockRunners,
  OLD_REVISION,
  SAVED_REVISION,
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
      makeRunner({ id: "runner-b", status: "standing_down", pr_slots_today: 5, prs_today: 2, daily_pr_limit: 5 }),
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
    expect(within(cardB).getByText("100%")).toBeInTheDocument();
    expect(within(cardB).getByRole("progressbar", { name: "UTC 准入槽位 / 本进程上报值" })).toHaveAttribute("value", "5");
    expect(within(cardB).getByText(/UTC 今日确认 PR（创建 \/ 本地观测记录）：2/)).toBeInTheDocument();
    expect(within(cardB).getByText(/不是中心统一硬上限/)).toHaveTextContent("各进程可能不同");
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

  it("shows mixed revisions and unknown startup data without conflating activation and liveness", async () => {
    signedIn([
      makeRunner({
        id: "current",
        offline: true,
        configuration: makeProcessConfiguration({
          freshness: "offline",
          snapshot: makeStartupConfig({ runner_id: "current" }),
        }),
      }),
      makeRunner({
        id: "old",
        configuration: makeProcessConfiguration({
          activation: "stale",
          snapshot: makeStartupConfig({
            runner_id: "old",
            process_instance_id: "66666666-6666-4666-8666-666666666666",
            loaded_revision: OLD_REVISION,
            values: { NUWA_INTERVAL: { value: "77", source: "file", reason: null } },
          }),
        }),
      }),
      makeRunner({
        id: "legacy",
        effective: { ...makeRunner().effective, interval_secs: 30 },
        configuration: makeProcessConfiguration({
          runner_id: "legacy",
          snapshot: null,
          activation: "unknown",
          reason: "startup_snapshot_unavailable",
        }),
      }),
    ]);
    renderApp("/runners");
    const current = await screen.findByRole("region", { name: "work / current 配置" });
    expect(within(current).getByText("配置状态：已加载保存版本（active）")).toBeInTheDocument();
    expect(within(current).getByText("上报状态：离线（offline）")).toBeInTheDocument();
    expect(within(current).getByText(SAVED_REVISION)).toBeInTheDocument();
    const old = screen.getByRole("region", { name: "work / old 配置" });
    expect(within(old).getByText("配置状态：旧版本（stale）")).toBeInTheDocument();
    expect(within(old).getByText("上报状态：在线（online）")).toBeInTheDocument();
    expect(within(old).getByText(OLD_REVISION)).toBeInTheDocument();
    fireEvent.click(within(old).getByText("启动时实际配置与来源"));
    expect(within(old).getByText("77 · 来源：文件（file）")).toBeInTheDocument();
    const legacy = screen.getByRole("region", { name: "work / legacy 配置" });
    expect(within(legacy).getByText(/未知（unknown），不能确认已生效/)).toBeInTheDocument();
    expect(within(legacy).getByText(/缺少启动快照/)).toBeInTheDocument();
    expect(within(legacy).queryByText(/来源：/)).not.toBeInTheDocument();
  });

  it("keeps slot usage visible without inventing an unreported limit", async () => {
    signedIn([
      makeRunner({ id: "runner-no-quota", daily_pr_limit: null, pr_slots_today: 4, prs_today: 3 }),
    ]);

    renderApp("/runners");

    const card = await screen.findByTestId("runner-card-runner-no-quota");
    expect(within(card).getByText("未上报准入预算")).toBeInTheDocument();
    expect(within(card).getByText("4/—")).toBeInTheDocument();
    expect(
      within(card).queryByRole("progressbar", { name: "UTC 准入槽位 / 本进程上报值" }),
    ).not.toBeInTheDocument();
  });

  it("shows different process observations against the same repo-wide slot count", async () => {
    signedIn([
      makeRunner({ id: "low", pr_slots_today: 4, prs_today: 1, daily_pr_limit: 5 }),
      makeRunner({ id: "high", pr_slots_today: 4, prs_today: 1, daily_pr_limit: 10 }),
    ]);
    renderApp("/runners");
    const low = within(await screen.findByTestId("runner-card-low"));
    const high = within(screen.getByTestId("runner-card-high"));
    expect(low.getByText("4/5")).toBeInTheDocument();
    expect(low.getByText("80%")).toBeInTheDocument();
    expect(high.getByText("4/10")).toBeInTheDocument();
    expect(high.getByText("40%")).toBeInTheDocument();
    expect(high.getByText(/策略由创建进程解析/)).toBeInTheDocument();
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
