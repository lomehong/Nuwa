import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  mockMe,
  mockOverviewResources,
  mockSettings,
  mockSystem,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

const health = vi.hoisted(() => ({
  waitForServeRecovery: vi.fn<() => Promise<void>>(),
  reloadConsole: vi.fn(),
}));

vi.mock("../../lib/health", () => ({
  waitForServeRecovery: health.waitForServeRecovery,
  probeServeOnce: vi.fn(),
  reloadConsole: health.reloadConsole,
}));

const ACTIVE_RUN = {
  id: "run-1",
  fingerprint: "fp-1",
  runner_id: "r1",
  repo: "a/b",
  phase: "repair",
  started_at_ms: Date.now(),
};

async function openRestart() {
  fireEvent.click(await screen.findByRole("button", { name: "重启服务" }));
  fireEvent.click(await screen.findByRole("button", { name: "确认重启" }));
}

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
  health.waitForServeRecovery.mockReset();
  health.waitForServeRecovery.mockResolvedValue(undefined);
  health.reloadConsole.mockReset();
});

describe("RestartDialog", () => {
  it("shows active runs on 409 and only enables force after confirmation", async () => {
    const bodies: unknown[] = [];
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.post("/api/v1/settings/restart", async ({ request }) => {
        const body = await request.json();
        bodies.push(body);
        if (!(body as { force?: boolean }).force) {
          return HttpResponse.json(
            {
              error: { code: "conflict", message: "active runs" },
              active_runs: [ACTIVE_RUN],
            },
            { status: 409 },
          );
        }
        return new HttpResponse(null, { status: 202 });
      }),
    );
    const reload = health.reloadConsole;
    renderApp("/settings");

    await openRestart();

    expect(await screen.findByText("fp-1")).toBeInTheDocument();
    expect(screen.getByText("r1")).toBeInTheDocument();
    expect(screen.getByText("repair")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "fp-1" })).toHaveAttribute(
      "href",
      "/issues/fp-1",
    );
    const force = screen.getByRole("button", { name: "强制重启" });
    expect(force).toBeDisabled();

    fireEvent.click(
      screen.getByRole("checkbox", { name: /我了解这会中断正在进行的修复/ }),
    );
    expect(force).toBeEnabled();
    fireEvent.click(force);

    await waitFor(() => expect(bodies).toEqual([{ force: false }, { force: true }]));
    await waitFor(() => expect(health.waitForServeRecovery).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(reload).toHaveBeenCalled());
  });

  it("shows the restarting status after a 202 and reloads once recovery lands", async () => {
    let body: unknown;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.post("/api/v1/settings/restart", async ({ request }) => {
        body = await request.json();
        return new HttpResponse(null, { status: 202 });
      }),
    );
    let resolveRecovery!: () => void;
    health.waitForServeRecovery.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          resolveRecovery = resolve;
        }),
    );
    const reload = health.reloadConsole;
    renderApp("/settings");

    await openRestart();

    expect(await screen.findByText("正在重启，等待服务恢复…")).toBeInTheDocument();
    expect(body).toEqual({ force: false });
    expect(health.waitForServeRecovery).toHaveBeenCalledTimes(1);

    await act(async () => {
      resolveRecovery();
    });
    await waitFor(() => expect(reload).toHaveBeenCalledTimes(1));
  });

  it("shows the 501 manual command instead of retrying blindly", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.post("/api/v1/settings/restart", () =>
        HttpResponse.json(
          {
            error: {
              code: "not_supported",
              message:
                "systemd-run is unavailable; restart the units manually: systemctl restart nuwa-serve nuwa-work",
            },
          },
          { status: 501 },
        ),
      ),
    );
    renderApp("/settings");

    await openRestart();

    expect(
      await screen.findByText(/systemctl restart nuwa-serve nuwa-work/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    expect(health.waitForServeRecovery).not.toHaveBeenCalled();
  });

  it("does not post on cancel or Escape", async () => {
    let posts = 0;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.post("/api/v1/settings/restart", () => {
        posts += 1;
        return new HttpResponse(null, { status: 202 });
      }),
    );
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "重启服务" }));
    await screen.findByRole("dialog", { name: "重启服务？" });
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "重启服务？" })).toBeNull());

    fireEvent.click(screen.getByRole("button", { name: "重启服务" }));
    const reopened = await screen.findByRole("dialog", { name: "重启服务？" });
    fireEvent.keyDown(reopened, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "重启服务？" })).toBeNull());

    expect(posts).toBe(0);
  });
});
