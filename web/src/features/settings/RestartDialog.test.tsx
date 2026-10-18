import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  mockMe,
  mockOverviewResources,
  mockRunners,
  mockSettings,
  mockSystem,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";
import {
  RECOVERY_TIMEOUT_MS,
  SessionExpiredDuringRecoveryError,
} from "../../lib/health";

const health = vi.hoisted(() => ({
  waitForServeRecovery: vi.fn<(signal?: AbortSignal) => Promise<void>>(),
  reloadConsole: vi.fn(),
  navigateToLogin: vi.fn(),
}));

vi.mock("../../lib/health", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../lib/health")>();
  return {
    ...actual,
    waitForServeRecovery: health.waitForServeRecovery,
    reloadConsole: health.reloadConsole,
    navigateToLogin: health.navigateToLogin,
  };
});

const ACTIVE_RUN = {
  id: "run-1",
  fingerprint: "fp-1",
  runner_id: "r1",
  repo: "a/b",
  phase: "repair",
  started_at_ms: Date.now(),
};

const PENDING_RESTART = "正在重启，等待服务恢复…";

async function openRestart() {
  fireEvent.click(await screen.findByRole("button", { name: "重启服务" }));
  fireEvent.click(await screen.findByRole("button", { name: "确认重启" }));
}

function signedIn(...handlers: Parameters<typeof server.use>) {
  server.use(
    mockMe(),
    ...mockOverviewResources(),
    mockSystem(),
    mockSettings(),
    mockRunners(),
    ...handlers,
  );
}

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
  health.waitForServeRecovery.mockReset();
  health.waitForServeRecovery.mockResolvedValue(undefined);
  health.reloadConsole.mockReset();
  health.navigateToLogin.mockReset();
});

describe("RestartDialog", () => {
  it("shows active runs on 409 and only enables force after confirmation", async () => {
    const bodies: unknown[] = [];
    const csrf: Array<string | null> = [];
    signedIn(
      http.post("/api/v1/settings/restart", async ({ request }) => {
        csrf.push(request.headers.get("x-nuwa-csrf"));
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
    // Both the guarded and the forced POST carry the double-submit token.
    expect(csrf).toEqual(["csrf-token", "csrf-token"]);
    await waitFor(() => expect(health.waitForServeRecovery).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(health.reloadConsole).toHaveBeenCalledTimes(1));
  });

  it("shows a focusable restarting status after a 202 and reloads once recovery lands", async () => {
    let body: unknown;
    let csrf: string | null = null;
    signedIn(
      http.post("/api/v1/settings/restart", async ({ request }) => {
        body = await request.json();
        csrf = request.headers.get("x-nuwa-csrf");
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

    const dialog = await screen.findByRole("dialog", { name: "重启服务" });
    const status = within(dialog).getByRole("status");
    expect(status).toHaveTextContent(PENDING_RESTART);
    // The panel has no focusable control while restarting; status takes focus
    // so assistive tech announces the wait.
    expect(status).toHaveFocus();
    expect(body).toEqual({ force: false });
    expect(csrf).toBe("csrf-token");
    expect(health.waitForServeRecovery).toHaveBeenCalledTimes(1);
    expect(health.waitForServeRecovery.mock.calls[0][0]).toBeInstanceOf(AbortSignal);

    await act(async () => {
      resolveRecovery();
    });
    await waitFor(() => expect(reload).toHaveBeenCalledTimes(1));
  });

  it("asks for a fresh login when the rotated token expires the session", async () => {
    signedIn(
      http.post("/api/v1/settings/restart", () => new HttpResponse(null, { status: 202 })),
    );
    health.waitForServeRecovery.mockRejectedValue(
      new SessionExpiredDuringRecoveryError(),
    );
    renderApp("/settings");

    await openRestart();

    expect(await screen.findByText("令牌已更改，请重新登录")).toBeInTheDocument();
    const login = screen.getByRole("button", { name: "重新登录" });
    fireEvent.click(login);

    expect(health.navigateToLogin).toHaveBeenCalledTimes(1);
    expect(health.reloadConsole).not.toHaveBeenCalled();
  });

  it("shows the 501 manual command without a retry that cannot succeed", async () => {
    signedIn(
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
    expect(screen.getByRole("button", { name: "关闭" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();
    expect(health.waitForServeRecovery).not.toHaveBeenCalled();
  });

  it("keeps a mutex 409 with no active runs retryable", async () => {
    const bodies: unknown[] = [];
    signedIn(
      http.post("/api/v1/settings/restart", async ({ request }) => {
        bodies.push(await request.json());
        return HttpResponse.json(
          {
            error: { code: "conflict", message: "another restart is in progress" },
            active_runs: [],
          },
          { status: 409 },
        );
      }),
    );
    renderApp("/settings");

    await openRestart();

    expect(await screen.findByText("another restart is in progress")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "强制重启" })).not.toBeInTheDocument();
    const retry = screen.getByRole("button", { name: "重试" });
    expect(retry).toBeEnabled();
    fireEvent.click(retry);
    await waitFor(() => expect(bodies).toEqual([{ force: false }, { force: false }]));
  });

  it("includes 422 field errors in the restart failure message", async () => {
    signedIn(
      http.post("/api/v1/settings/restart", () =>
        HttpResponse.json(
          {
            error: {
              code: "validation_failed",
              message: "settings are invalid",
              fields: { NUWA_SERVE_UNIT: "unit must end in .service" },
            },
          },
          { status: 422 },
        ),
      ),
    );
    renderApp("/settings");

    await openRestart();

    expect(
      await screen.findByText("settings are invalid（NUWA_SERVE_UNIT：unit must end in .service）"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("surfaces a recovery timeout using the exported limit and allows retrying", async () => {
    const bodies: unknown[] = [];
    signedIn(
      http.post("/api/v1/settings/restart", async ({ request }) => {
        bodies.push(await request.json());
        return new HttpResponse(null, { status: 202 });
      }),
    );
    health.waitForServeRecovery.mockRejectedValue(new Error("服务未恢复"));
    renderApp("/settings");

    await openRestart();

    const seconds = RECOVERY_TIMEOUT_MS / 1_000;
    expect(
      await screen.findByText(
        `服务在 ${seconds} 秒内未恢复，请检查 systemd 状态：systemctl status nuwa-serve nuwa-work`,
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(bodies).toEqual([{ force: false }, { force: false }]));
  });

  it("ignores cancel and Escape while the restart POST is pending and still handles the 202", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let posts = 0;
    signedIn(
      http.post("/api/v1/settings/restart", async () => {
        posts += 1;
        await gate;
        return new HttpResponse(null, { status: 202 });
      }),
    );
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "重启服务" }));
    fireEvent.click(await screen.findByRole("button", { name: "确认重启" }));

    const dialog = await screen.findByRole("dialog", { name: "重启服务？" });
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "取消" })).toBeDisabled());
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    fireEvent.keyDown(dialog, { key: "Escape" });
    fireEvent.mouseDown(dialog.parentElement!);
    expect(screen.getByRole("dialog", { name: "重启服务？" })).toBeInTheDocument();

    release();

    expect(await screen.findByText(PENDING_RESTART)).toBeInTheDocument();
    await waitFor(() => expect(health.waitForServeRecovery).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(health.reloadConsole).toHaveBeenCalledTimes(1));
    expect(posts).toBe(1);
  });

  it("aborts the recovery poll when the dialog unmounts", async () => {
    let captured: AbortSignal | undefined;
    signedIn(
      http.post("/api/v1/settings/restart", () => new HttpResponse(null, { status: 202 })),
    );
    health.waitForServeRecovery.mockImplementation((signal) => {
      captured = signal;
      return new Promise<void>((_, reject) => {
        signal?.addEventListener("abort", () =>
          reject(new DOMException("aborted", "AbortError")),
        );
      });
    });
    const { unmount } = renderApp("/settings");

    await openRestart();
    expect(await screen.findByText(PENDING_RESTART)).toBeInTheDocument();
    expect(captured?.aborted).toBe(false);

    unmount();

    expect(captured?.aborted).toBe(true);
    expect(health.reloadConsole).not.toHaveBeenCalled();
  });

  it("does not post on cancel or Escape", async () => {
    let posts = 0;
    signedIn(
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
