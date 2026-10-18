import { fireEvent, screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIssues, mockMe, mockOverviewResources, server } from "../../test/msw";
import { renderApp } from "../../test/render";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
});

function signedIn() {
  server.use(mockMe(), ...mockOverviewResources(), mockIssues());
}

/** A realistic content-addressed fingerprint: far longer than a toast line. */
const LONG_FP = "0123456789abcdef".repeat(4);

describe("SyncButton", () => {
  it("syncs with CSRF and reports the summary plus per-issue errors", async () => {
    let csrf: string | null = null;
    let body: unknown;
    server.use(
      http.post("/api/v1/sync", async ({ request }) => {
        csrf = request.headers.get("x-nuwa-csrf");
        body = await request.json();
        return HttpResponse.json({
          checked: 3,
          merged: 1,
          rejected: 0,
          still_open: 1,
          errors: [{ fingerprint: "fp-2", message: "GitHub 502" }],
        });
      }),
    );
    signedIn();

    renderApp("/issues");
    fireEvent.click(await screen.findByRole("button", { name: "GitHub 对账" }));

    expect(
      await screen.findByText(/检查 3 · 合并 1 · 拒绝 0 · 仍未合并 1/),
    ).toBeInTheDocument();
    expect(screen.getByText(/1 个查询失败/)).toBeInTheDocument();
    expect(screen.getByText("fp-2 — GitHub 502")).toBeInTheDocument();
    expect(csrf).toBe("csrf-token");
    expect(body).toEqual({});
  });

  it("renders preformatted error strings without an object prefix", async () => {
    server.use(
      http.post("/api/v1/sync", () =>
        HttpResponse.json({
          checked: 1,
          merged: 0,
          rejected: 0,
          still_open: 1,
          errors: ["fp-3 — GitHub timeout"],
        }),
      ),
    );
    signedIn();

    renderApp("/issues");
    fireEvent.click(await screen.findByRole("button", { name: "GitHub 对账" }));

    expect(await screen.findByText("fp-3 — GitHub timeout")).toBeInTheDocument();
    expect(screen.queryByText(/\[object Object\]/)).not.toBeInTheDocument();
  });

  it("wraps a 64-char fingerprint in the per-issue error toast", async () => {
    server.use(
      http.post("/api/v1/sync", () =>
        HttpResponse.json({
          checked: 1,
          merged: 0,
          rejected: 0,
          still_open: 1,
          errors: [{ fingerprint: LONG_FP, message: "GitHub 502" }],
        }),
      ),
    );
    signedIn();

    renderApp("/issues");
    fireEvent.click(await screen.findByRole("button", { name: "GitHub 对账" }));

    const error = await screen.findByText(`${LONG_FP} — GitHub 502`);
    expect(error).toHaveClass("break-words");
    expect(error).toHaveTextContent(LONG_FP);
  });

  it("accounts for errors hidden beyond the first three toasts", async () => {
    server.use(
      http.post("/api/v1/sync", () =>
        HttpResponse.json({
          checked: 5,
          merged: 0,
          rejected: 0,
          still_open: 5,
          errors: Array.from({ length: 5 }, (_, index) => `fp-${index + 1} — GitHub 502`),
        }),
      ),
    );
    signedIn();

    renderApp("/issues");
    fireEvent.click(await screen.findByRole("button", { name: "GitHub 对账" }));

    expect(await screen.findByText(/5 个查询失败；另有 2 条/)).toBeInTheDocument();
    expect(screen.getAllByText("对账查询失败")).toHaveLength(3);
  });

  it.each([502, 500])("surfaces a %s from the sync endpoint", async (status) => {
    server.use(
      http.post("/api/v1/sync", () =>
        HttpResponse.json(
          {
            error: {
              code: "github_error",
              message: `GitHub upstream returned ${status}`,
            },
          },
          { status },
        ),
      ),
    );
    signedIn();

    renderApp("/issues");
    fireEvent.click(await screen.findByRole("button", { name: "GitHub 对账" }));

    expect(await screen.findByText("对账失败")).toBeInTheDocument();
    expect(screen.getByText(`GitHub upstream returned ${status}`)).toBeInTheDocument();
  });

  it("explains a concurrent reconcile on 409", async () => {
    server.use(
      http.post("/api/v1/sync", () =>
        HttpResponse.json(
          { error: { code: "conflict", message: "sync is already running" } },
          { status: 409 },
        ),
      ),
    );
    signedIn();

    renderApp("/issues");
    fireEvent.click(await screen.findByRole("button", { name: "GitHub 对账" }));

    expect(await screen.findByText("对账失败")).toBeInTheDocument();
    expect(
      screen.getByText("已有对账任务在运行，请稍后重试。"),
    ).toBeInTheDocument();
  });
});
