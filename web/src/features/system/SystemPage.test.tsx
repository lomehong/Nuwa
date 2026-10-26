import { fireEvent, screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import {
  mockMe,
  mockOverviewResources,
  mockSystem,
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
