import { fireEvent, screen, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { THEME_STORAGE_KEY } from "../lib/theme";
import { makeOverview, mockMe, mockOverview, server } from "../test/msw";
import { renderApp } from "../test/render";

function signedIn(overrides: Parameters<typeof makeOverview>[0] = {}) {
  server.use(mockMe(), mockOverview(overrides));
}

beforeEach(() => {
  window.localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
});

afterEach(() => {
  window.localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
  document.cookie = "nuwa_csrf=; Max-Age=0; path=/";
});

describe("AppShell", () => {
  it("renders the sidebar nav and marks future pages as Soon", async () => {
    signedIn();
    renderApp("/");

    const nav = await screen.findByRole("navigation", { name: "主导航" });
    expect(within(nav).getByRole("link", { name: "总览" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(within(nav).getByRole("link", { name: "问题单" })).toBeInTheDocument();

    for (const label of ["运行器", "活动", "设置"]) {
      const item = within(nav).getByText(label).closest("[aria-disabled='true']");
      expect(item).not.toBeNull();
    }
    expect(within(nav).getAllByText("Soon")).toHaveLength(3);
  });

  it("renders the health strip from /overview", async () => {
    signedIn({
      queue: { ready_count: 3, next: [] },
      active_runs: [
        {
          id: "run-1",
          fingerprint: "fp-1",
          title: "boom",
          repo: "example.com/app",
          runner_id: "runner-1",
          phase: "repair",
          started_at_ms: 0,
          turns: 2,
          tokens: 1200,
        },
      ],
      health: {
        runners_total: 2,
        runners_online: 1,
        working: 1,
        idle: 0,
        standing_down: 1,
        error: 0,
        reports_today: 4,
        prs_today: 2,
      },
    });

    renderApp("/");

    const region = await screen.findByRole("region", { name: "系统健康" });
    expect(await within(region).findByText("serve 正常")).toBeInTheDocument();
    expect(within(region).getByText("在线")).toBeInTheDocument();
    expect(within(region).getByText("1/2")).toBeInTheDocument();
    expect(within(region).getByRole("link", { name: /^队列\s*3$/ })).toBeInTheDocument();
    expect(within(region).getByRole("link", { name: /^运行中\s*1$/ })).toBeInTheDocument();
    expect(within(region).getByText("今日 PR").parentElement).toHaveTextContent("今日 PR2");
    expect(within(region).getByText("配额待命").parentElement).toHaveTextContent("配额待命1");
  });

  it("logs out through the API and returns to the login page", async () => {
    signedIn();
    document.cookie = "nuwa_csrf=logout-token; path=/";
    let csrfHeader: string | null = null;
    server.use(
      http.post("/api/v1/auth/logout", ({ request }) => {
        csrfHeader = request.headers.get("x-nuwa-csrf");
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderApp("/");

    fireEvent.click(await screen.findByRole("button", { name: "退出登录" }));

    expect(await screen.findByRole("heading", { name: "女娲 · 补天" })).toBeInTheDocument();
    expect(csrfHeader).toBe("logout-token");
  });

  it("keeps the theme toggle working inside the shell", async () => {
    document.documentElement.setAttribute("data-theme", "dark");
    signedIn();
    renderApp("/");

    fireEvent.click(await screen.findByRole("button", { name: "切换至浅色主题" }));

    expect(document.documentElement).toHaveAttribute("data-theme", "light");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });

  it("renders the mobile bottom navigation without inline styles", async () => {
    signedIn();
    const { container } = renderApp("/");

    const mobileNav = await screen.findByRole("navigation", { name: "移动导航" });
    expect(within(mobileNav).getByRole("link", { name: "总览" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(within(mobileNav).getByRole("link", { name: "问题单" })).toBeInTheDocument();
    expect(within(mobileNav).getByText(/^运行器/)).toBeInTheDocument();
    expect(within(mobileNav).getByText(/^活动/)).toBeInTheDocument();
    expect(container.querySelector("[style]")).toBeNull();
  });

  it("shows local mode instead of a logout action", async () => {
    server.use(mockMe({ local_mode: true, expires_at_ms: null }), mockOverview());
    renderApp("/");

    expect(await screen.findByText("本地模式")).toBeInTheDocument();
    expect(screen.getByText("本地模式 · API 无鉴权")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "退出登录" })).not.toBeInTheDocument();
  });
});
