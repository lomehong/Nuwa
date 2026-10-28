import { fireEvent, screen, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import {
  makeProcessConfiguration,
  makeStartupConfig,
  makeSystem,
  mockMe,
  mockOverviewResources,
  mockSystem,
  OLD_REVISION,
  SAVED_REVISION,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

function signedIn(system: Parameters<typeof mockSystem>[0] = {}) {
  server.use(mockMe(), ...mockOverviewResources(), mockSystem(system));
}

describe("SystemPage", () => {
  it("shows runtime status, units, maintenance and about", async () => {
    signedIn();
    renderApp("/system");

    expect(
      await screen.findByRole("heading", { level: 2, name: "运行状态" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: "服务" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 2, name: "服务单元" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 2, name: "关于" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 2, name: "维护" })).toBeInTheDocument();

    expect(screen.getAllByText("0.2.0").length).toBeGreaterThan(0);
    expect(screen.getByText("0123456")).toBeInTheDocument();
    expect(screen.getByText("nuwa-serve")).toBeInTheDocument();
    expect(screen.getByText("nuwa-work")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "GitHub 对账" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重启服务" })).toBeInTheDocument();

    const source = screen.getByRole("link", { name: "https://github.com/lomehong/nuwa" });
    expect(source).toHaveAttribute("href", "https://github.com/lomehong/nuwa");
    expect(source).toHaveAttribute("rel", "noreferrer");
  });

  it("warns when the console runs in local mode", async () => {
    server.use(
      mockMe({ local_mode: true, expires_at_ms: null }),
      ...mockOverviewResources(),
      mockSystem({ local_mode: true }),
    );
    renderApp("/system");

    expect(
      await screen.findByText(/任何能访问该端口的客户端都可修改设置或重启服务/),
    ).toBeInTheDocument();
  });

  it("shows only this serve's startup evidence and refreshes its activation independently", async () => {
    let current = makeSystem({ configuration: makeProcessConfiguration({
      activation: "stale",
      snapshot: makeStartupConfig({ role: "serve", loaded_revision: OLD_REVISION }),
    }) });
    signedIn();
    server.use(http.get("/api/v1/system", () => HttpResponse.json(current)));
    renderApp("/system");
    const serve = await screen.findByRole("region", { name: "serve 配置" });
    expect(within(serve).getByText("配置状态：旧版本（stale）")).toBeInTheDocument();
    expect(within(serve).getByText("上报状态：在线（online）")).toBeInTheDocument();
    expect(within(serve).getByText(OLD_REVISION)).toBeInTheDocument();
    fireEvent.click(within(serve).getByText("启动时实际配置与来源"));
    expect(within(serve).getByText("auto · 来源：默认值（default）")).toBeInTheDocument();
    expect(within(serve).getByText("/opt/nuwa/nuwa.env · 来源：推导值（derived）")).toBeInTheDocument();
    expect(screen.getByText(/重启 serve 不代表其他 work 已采用保存版本/)).toBeInTheDocument();

    current = makeSystem({ configuration: makeProcessConfiguration({
      snapshot: makeStartupConfig({ role: "serve", process_instance_id: "77777777-7777-4777-8777-777777777777" }),
    }) });
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await within(serve).findByText("配置状态：已加载保存版本（active）")).toBeInTheDocument();
    expect(within(serve).getByText(SAVED_REVISION)).toBeInTheDocument();
    expect(within(serve).getByText("77777777-7777-4777-8777-777777777777")).toBeInTheDocument();
  });

  it("does not infer configuration activation from active systemd units", async () => {
    signedIn({ configuration: makeProcessConfiguration({
      role: "serve",
      runner_id: null,
      snapshot: null,
      activation: "unknown",
      reason: "startup_snapshot_unavailable",
    }) });
    renderApp("/system");
    const serve = await screen.findByRole("region", { name: "serve 配置" });
    expect(within(serve).getByText("配置状态：未知（unknown），不能确认已生效")).toBeInTheDocument();
    expect(within(serve).getByText("上报状态：在线（online）")).toBeInTheDocument();
    expect(within(serve).getByText(/缺少启动快照/)).toBeInTheDocument();
    expect(within(serve).queryByText(/配置状态：已加载/)).not.toBeInTheDocument();
  });

  it("explains a missing systemd unit", async () => {
    signedIn({ services: [] });
    renderApp("/system");

    expect(await screen.findByText("未配置 systemd unit。")).toBeInTheDocument();
    expect(screen.getByText(/将重启：未配置 systemd unit。/)).toBeInTheDocument();
  });

  it("offers a retry when the system snapshot fails", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      http.get("/api/v1/system", () =>
        HttpResponse.json(
          { error: { code: "internal", message: "system store unavailable" } },
          { status: 500 },
        ),
      ),
    );
    renderApp("/system");

    expect(await screen.findByText("无法加载服务状态")).toBeInTheDocument();
    const retry = screen.getByRole("button", { name: "重试" });
    expect(retry).toBeInTheDocument();
    fireEvent.click(retry);
  });
});
