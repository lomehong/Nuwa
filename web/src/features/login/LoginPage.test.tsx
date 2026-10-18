import { fireEvent, screen, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIssues, mockMe, mockOverview, mockOverviewResources, mockUnauthorizedMe, server } from "../../test/msw";
import { renderApp } from "../../test/render";

beforeEach(() => {
  // `document.title` survives between tests in the same jsdom environment;
  // reset it so this assertion cannot pass on a previous test's value.
  document.title = "";
});

describe("LoginPage", () => {
  it("sets the login document title", async () => {
    server.use(mockUnauthorizedMe());

    renderApp("/login");

    await screen.findByLabelText("访问令牌");
    // The title is set from an effect, so allow it to flush after the paint.
    await waitFor(() => expect(document.title).toBe("登录 · 女娲"));
  });

  it("signs in and returns to the intended route", async () => {
    let received: unknown = null;
    server.use(
      mockUnauthorizedMe(),
      http.post("/api/v1/auth/login", async ({ request }) => {
        received = await request.json();
        return new HttpResponse(null, { status: 204 });
      }),
      mockOverview(),
      mockIssues(),
    );

    renderApp({ pathname: "/login", state: { from: "/issues" } });

    const input = await screen.findByLabelText("访问令牌");
    fireEvent.change(input, { target: { value: "ui-token" } });
    fireEvent.click(screen.getByRole("button", { name: "进入控制台" }));

    expect(await screen.findByText("暂无问题单")).toBeInTheDocument();
    expect(received).toEqual({ token: "ui-token" });
  });

  it("shows an error for a wrong token and stays on the login page", async () => {
    server.use(
      mockUnauthorizedMe(),
      http.post("/api/v1/auth/login", () =>
        HttpResponse.json(
          { error: { code: "unauthorized", message: "invalid token" } },
          { status: 401 },
        ),
      ),
    );

    renderApp("/login");

    fireEvent.change(await screen.findByLabelText("访问令牌"), {
      target: { value: "wrong-token" },
    });
    fireEvent.click(screen.getByRole("button", { name: "进入控制台" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("令牌不正确");
    expect(screen.getByLabelText("访问令牌")).toBeInTheDocument();
  });

  it("reports rate limiting with the Retry-After seconds", async () => {
    server.use(
      mockUnauthorizedMe(),
      http.post("/api/v1/auth/login", () =>
        HttpResponse.json(
          { error: { code: "rate_limited", message: "retry after 45000ms" } },
          { status: 429, headers: { "Retry-After": "45" } },
        ),
      ),
    );

    renderApp("/login");

    fireEvent.change(await screen.findByLabelText("访问令牌"), {
      target: { value: "wrong-token" },
    });
    fireEvent.click(screen.getByRole("button", { name: "进入控制台" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("45 秒后重试");
  });

  it("skips the form in local mode and goes straight to the overview", async () => {
    server.use(mockMe({ local_mode: true, expires_at_ms: null }), ...mockOverviewResources());

    renderApp("/login");

    expect(
      await screen.findByRole("heading", { level: 2, name: "总览" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "女娲 · 补天" }),
    ).not.toBeInTheDocument();
  });
});
