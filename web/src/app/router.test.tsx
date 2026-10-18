import { fireEvent, screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import {
  makeIssue,
  mockEvents,
  mockIssueDetail,
  mockIssues,
  mockMe,
  mockOverviewResources,
  mockTranscript,
  mockTrends,
  mockUnauthorizedMe,
  server,
} from "../test/msw";
import { renderApp } from "../test/render";

describe("AppRoutes", () => {
  it("redirects unauthenticated visitors to /login", async () => {
    server.use(mockUnauthorizedMe());

    renderApp("/");

    expect(
      await screen.findByRole("heading", { name: "女娲 · 补天" }),
    ).toBeInTheDocument();
  });

  it("redirects to /login when a protected request returns 401", async () => {
    server.use(
      mockMe(),
      http.get("/api/v1/overview", () =>
        HttpResponse.json(
          { error: { code: "unauthorized", message: "session expired" } },
          { status: 401 },
        ),
      ),
      mockTrends(),
      mockEvents(),
    );

    renderApp("/");

    expect(
      await screen.findByRole("heading", { name: "女娲 · 补天" }),
    ).toBeInTheDocument();
  });

  it("shows a retryable boot error instead of the login form when /auth/me is unreachable", async () => {
    let fail = true;
    server.use(
      http.get("/api/v1/auth/me", () =>
        fail
          ? HttpResponse.error()
          : HttpResponse.json({
              authenticated: true,
              local_mode: false,
              expires_at_ms: 1_700_000_000_000,
            }),
      ),
      ...mockOverviewResources(),
    );

    renderApp("/");

    expect(
      await screen.findByRole("heading", { name: "无法连接服务器" }),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("访问令牌")).not.toBeInTheDocument();

    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "重试连接" }));

    expect(
      await screen.findByRole("heading", { level: 2, name: "总览" }),
    ).toBeInTheDocument();
  });

  it("returns unknown paths to the overview", async () => {
    server.use(mockMe(), ...mockOverviewResources());

    renderApp("/does-not-exist");

    expect(
      await screen.findByRole("heading", { level: 2, name: "总览" }),
    ).toBeInTheDocument();
  });

  it("renders the issues list", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockIssues({
        data: [makeIssue()],
        meta: { page: 1, page_size: 50, total: 1 },
      }),
    );

    renderApp("/issues");

    expect(
      await screen.findByRole("heading", { level: 1, name: "问题单" }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("region", { name: "问题单列表" }),
    ).toBeInTheDocument();
  });

  it("renders the issue detail with the fingerprint from the route", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockIssueDetail({
        issue: makeIssue({
          fingerprint: "fp-123",
          title: "Router detail issue",
        }),
      }),
      mockTranscript(),
    );

    renderApp("/issues/fp-123");

    expect(
      await screen.findByRole("heading", { level: 1, name: "问题单详情" }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { level: 2, name: "Router detail issue" }),
    ).toBeInTheDocument();
    expect(screen.getAllByText("fp-123").length).toBeGreaterThan(0);
  });
});
