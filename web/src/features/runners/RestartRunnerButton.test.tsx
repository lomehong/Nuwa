import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  makeRunner,
  mockMe,
  mockOverviewResources,
  mockRunners,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
});

function signedIn(runners = [makeRunner()]) {
  server.use(mockMe(), ...mockOverviewResources(), mockRunners(runners));
}

/** A realistic content-addressed fingerprint: far longer than the dialog line. */
const LONG_FP = "0123456789abcdef".repeat(4);

describe("RestartRunnerButton", () => {
  it("warns about the active issue and surfaces a 501 manual command", async () => {
    signedIn([makeRunner({ id: "runner-a", current_fingerprint: "fp-1" })]);
    server.use(
      http.post("/api/v1/runners/runner-a/restart", () =>
        HttpResponse.json(
          {
            error: {
              code: "not_supported",
              message:
                "systemd-run is unavailable; restart manually: systemctl restart nuwa-work",
            },
          },
          { status: 501 },
        ),
      ),
    );

    renderApp("/runners");
    fireEvent.click(
      await screen.findByRole("button", { name: "重启 unit：runner-a" }),
    );

    const dialog = screen.getByRole("dialog", { name: "重启 runner unit？" });
    expect(within(dialog).getByText(/服务单元：nuwa-work/)).toBeInTheDocument();
    expect(within(dialog).getByText(/当前正在修复 fp-1，重启会中断该 run/)).toBeInTheDocument();
    expect(
      within(dialog).getByText(/同一 unit 承载的多个 runner 会一起重启/),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "确认重启" }));

    expect(
      await screen.findByText(/systemctl restart nuwa-work/),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("shows the full 64-char fingerprint and wraps it in the dialog", async () => {
    signedIn([makeRunner({ id: "runner-long", current_fingerprint: LONG_FP })]);

    renderApp("/runners");
    fireEvent.click(
      await screen.findByRole("button", { name: "重启 unit：runner-long" }),
    );

    const dialog = screen.getByRole("dialog", { name: "重启 runner unit？" });
    const description = within(dialog).getByText(new RegExp(LONG_FP));
    expect(description).toHaveClass("break-words");
    expect(description).toHaveTextContent(`当前正在修复 ${LONG_FP}，重启会中断该 run。`);
  });

  it("explains a concurrent restart on 409", async () => {
    signedIn();
    server.use(
      http.post("/api/v1/runners/:id/restart", () =>
        HttpResponse.json(
          { error: { code: "conflict", message: "restart already scheduled" } },
          { status: 409 },
        ),
      ),
    );

    renderApp("/runners");
    fireEvent.click(await screen.findByRole("button", { name: "重启 unit：runner-1" }));
    fireEvent.click(screen.getByRole("button", { name: "确认重启" }));

    expect(await screen.findByText("重启失败")).toBeInTheDocument();
    expect(
      screen.getByText("已有重启任务在排程，请稍后重试。"),
    ).toBeInTheDocument();
  });

  it("cancels without posting", async () => {
    let posts = 0;
    signedIn();
    server.use(
      http.post("/api/v1/runners/:id/restart", () => {
        posts += 1;
        return new HttpResponse(null, { status: 202 });
      }),
    );

    renderApp("/runners");
    fireEvent.click(
      await screen.findByRole("button", { name: "重启 unit：runner-1" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "取消" }));

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(posts).toBe(0);
  });

  it("schedules a restart with CSRF, toasts and invalidates fleet views", async () => {
    let csrf: string | null = null;
    let body: unknown;
    signedIn();
    server.use(
      http.post("/api/v1/runners/:id/restart", async ({ request }) => {
        csrf = request.headers.get("x-nuwa-csrf");
        body = await request.json();
        return new HttpResponse(null, { status: 202 });
      }),
    );

    const { queryClient } = renderApp("/runners");
    const invalidate = vi.spyOn(queryClient, "invalidateQueries");
    fireEvent.click(await screen.findByRole("button", { name: "重启 unit：runner-1" }));
    fireEvent.click(screen.getByRole("button", { name: "确认重启" }));

    expect(await screen.findByText("重启已排程")).toBeInTheDocument();
    expect(csrf).toBe("csrf-token");
    expect(body).toEqual({});
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());

    const keys = invalidate.mock.calls.map(([filters]) => filters?.queryKey);
    expect(keys).toEqual(expect.arrayContaining([["runners"], ["overview"]]));
  });

  it("shows the service_unit field detail on 422", async () => {
    signedIn();
    server.use(
      http.post("/api/v1/runners/:id/restart", () =>
        HttpResponse.json(
          {
            error: {
              code: "validation",
              message: "invalid request",
              fields: {
                service_unit: "service_unit may only contain letters, digits and @ . _ : -",
              },
            },
          },
          { status: 422 },
        ),
      ),
    );

    renderApp("/runners");
    fireEvent.click(await screen.findByRole("button", { name: "重启 unit：runner-1" }));
    fireEvent.click(screen.getByRole("button", { name: "确认重启" }));

    expect(await screen.findByText("重启失败")).toBeInTheDocument();
    expect(
      screen.getByText(/service_unit：service_unit may only contain/),
    ).toBeInTheDocument();
  });

  it("disables the action with an explanation when no unit is reported", async () => {
    let posts = 0;
    signedIn([makeRunner({ id: "runner-plain", service_unit: null })]);
    server.use(
      http.post("/api/v1/runners/:id/restart", () => {
        posts += 1;
        return new HttpResponse(null, { status: 202 });
      }),
    );

    renderApp("/runners");
    const button = await screen.findByRole("button", {
      name: "重启 unit：runner-plain",
    });

    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("title", "该 runner 未上报 service_unit，无法重启。");
    const hint = screen.getByText("未上报 service_unit");
    expect(hint).toBeVisible();
    expect(button).toHaveAttribute("aria-describedby", hint.id);
    fireEvent.click(button);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(posts).toBe(0);
  });
});
