import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { THEME_STORAGE_KEY } from "../lib/theme";
import {
  makeOverview,
  makeProject,
  mockIssues,
  mockMe,
  mockOverview,
  mockOverviewResources,
  mockProjects,
  mockRunners,
  mockSettings,
  mockSystem,
  mockTrends,
  mockEvents,
  server,
} from "../test/msw";
import { renderApp } from "../test/render";

function signedIn(overrides: Parameters<typeof makeOverview>[0] = {}) {
  server.use(mockMe(), ...mockOverviewResources(overrides));
}

/**
 * The desktop field and the mobile icon share the accessible name
 * "搜索问题单"; only the desktop field carries the visible label text.
 */
function desktopSearchTrigger(): HTMLElement {
  const trigger = screen
    .getAllByRole("button", { name: "搜索问题单" })
    .find((button) => button.textContent?.includes("搜索问题单…"));
  if (!trigger) {
    throw new Error("desktop search trigger not found");
  }
  return trigger;
}

function mobileSearchTrigger(): HTMLElement {
  const trigger = screen
    .getAllByRole("button", { name: "搜索问题单" })
    .find((button) => !button.textContent?.includes("搜索问题单…"));
  if (!trigger) {
    throw new Error("mobile search trigger not found");
  }
  return trigger;
}

beforeEach(() => {
  window.localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
  // `document.title` survives between tests in the same jsdom environment;
  // reset it so a title assertion cannot pass on a previous test's value.
  document.title = "";
});

afterEach(() => {
  window.localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
  document.cookie = "nuwa_csrf=; Max-Age=0; path=/";
});

describe("AppShell", () => {
  it("renders the sidebar nav with settings as an active route", async () => {
    signedIn();
    renderApp("/");

    const nav = await screen.findByRole("navigation", { name: "主导航" });
    expect(within(nav).getByRole("link", { name: "总览" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(within(nav).getByRole("link", { name: "问题单" })).toBeInTheDocument();
    expect(within(nav).getByRole("link", { name: "运行器" })).toHaveAttribute(
      "href",
      "/runners",
    );
    expect(within(nav).getByRole("link", { name: "活动" })).toHaveAttribute(
      "href",
      "/events",
    );
    expect(within(nav).getByRole("link", { name: "设置" })).toHaveAttribute(
      "href",
      "/settings",
    );
    expect(within(nav).queryByText("Soon")).not.toBeInTheDocument();
  });

  it.each([
    ["/", "总览"],
    ["/issues", "问题单"],
    ["/runners", "运行器"],
    ["/events", "活动"],
    ["/settings", "设置"],
  ] as const)("sets the document title on %s", async (route, title) => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockIssues(),
      mockRunners(),
      mockSettings(),
      mockSystem(),
    );
    renderApp(route);

    expect(
      await screen.findByRole("heading", { level: 1, name: title }),
    ).toBeInTheDocument();
    // The title is set from an effect, so allow it to flush after the paint.
    await waitFor(() => expect(document.title).toBe(`${title} · 女娲`));
  });

  it("updates the document title after client-side navigation", async () => {
    signedIn();
    server.use(mockIssues());
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });
    await waitFor(() => expect(document.title).toBe("总览 · 女娲"));

    const nav = screen.getByRole("navigation", { name: "主导航" });
    fireEvent.click(within(nav).getByRole("link", { name: "问题单" }));

    await screen.findByRole("heading", { level: 2, name: "问题单" });
    await waitFor(() => expect(document.title).toBe("问题单 · 女娲"));
  });

  it("marks the settings nav link current on /settings", async () => {
    server.use(mockMe(), ...mockOverviewResources(), mockSystem(), mockSettings());
    renderApp("/settings");

    const nav = await screen.findByRole("navigation", { name: "主导航" });
    expect(within(nav).getByRole("link", { name: "设置" })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });

  it("renders the runners route with the page title and placeholder", async () => {
    signedIn();
    renderApp("/runners");

    expect(await screen.findByRole("heading", { level: 1, name: "运行器" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 2, name: "运行器" })).toBeInTheDocument();
  });

  it("renders the events route with the page title and placeholder", async () => {
    signedIn();
    renderApp("/events");

    expect(await screen.findByRole("heading", { level: 1, name: "活动" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 2, name: "活动" })).toBeInTheDocument();
  });

  it("renders the status line from /overview", async () => {
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

    const region = await screen.findByRole("region", { name: "全局状态" });
    expect(await within(region).findByText("serve 正常")).toBeInTheDocument();
    expect(within(region).getByText("在线")).toBeInTheDocument();
    expect(within(region).getByText("1/2")).toBeInTheDocument();
    expect(within(region).getByText("全部项目")).toBeInTheDocument();
    expect(
      within(region).getByRole("link", { name: /^队列\s*3$/ }),
    ).toHaveAttribute("href", "/issues?state=new");
    expect(
      within(region).getByRole("link", { name: /^运行中\s*1$/ }),
    ).toHaveAttribute("href", "/issues?state=triggered,fixing");
    expect(within(region).getByText("今日 PR").parentElement).toHaveTextContent("今日 PR2");
    expect(within(region).getByText("配额待命").parentElement).toHaveTextContent("配额待命1");
    expect(within(region).getByRole("link", { name: /^在线\s*1\/2$/ })).toHaveAttribute(
      "href",
      "/runners",
    );
    expect(within(region).getByRole("link", { name: /^配额待命\s*1$/ })).toHaveAttribute(
      "href",
      "/runners",
    );
  });

  it("scopes the status line to the project in view", async () => {
    server.use(
      mockMe(),
      mockOverview(),
      mockTrends(),
      mockEvents(),
      mockIssues(),
      mockRunners(),
      mockProjects([makeProject({ repo: "example.com/app", name: "App" })]),
    );
    renderApp("/projects/example.com%2Fapp");

    const region = await screen.findByRole("region", { name: "全局状态" });
    expect(await within(region).findByText("App")).toBeInTheDocument();
    expect(within(region).getByText("取消限定")).toHaveAttribute("href", "/");
  });

  // WHY: the overview's `queue.ready_count` is the GLOBAL claim queue, so a
  // project-scoped strip must read the project's own `queued` instead. Alpha has
  // none while Beta (elsewhere) holds the whole global backlog; the scoped strip
  // must show 0 for Alpha, never the global 7.
  it("shows the project's own queue, not the global claim queue", async () => {
    server.use(
      mockMe(),
      mockOverview({ queue: { ready_count: 7, next: [] } }),
      mockTrends(),
      mockEvents(),
      mockIssues(),
      mockRunners(),
      mockProjects([
        makeProject({ repo: "example.com/alpha", name: "Alpha", queued: 0 }),
        makeProject({ repo: "example.com/beta", name: "Beta", queued: 7 }),
      ]),
    );
    renderApp("/projects/example.com%2Falpha");

    const region = await screen.findByRole("region", { name: "全局状态" });
    expect(await within(region).findByText("Alpha")).toBeInTheDocument();
    expect(
      within(region).getByRole("link", { name: /^队列\s*0$/ }),
    ).toHaveAttribute("href", "/issues?state=new&repo=example.com%2Falpha");
    expect(
      within(region).queryByRole("link", { name: /^队列\s*7$/ }),
    ).not.toBeInTheDocument();
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

  it("shows the server message when logout is rejected", async () => {
    signedIn();
    document.cookie = "nuwa_csrf=logout-token; path=/";
    server.use(
      http.post("/api/v1/auth/logout", () =>
        HttpResponse.json(
          { error: { code: "forbidden", message: "csrf check failed" } },
          { status: 403 },
        ),
      ),
    );

    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });
    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));

    expect(await screen.findByText("csrf check failed")).toBeInTheDocument();
    expect(screen.getByText("退出失败")).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "女娲 · 补天" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: "总览" })).toBeInTheDocument();
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
    expect(within(mobileNav).getByRole("link", { name: "运行器" })).toHaveAttribute(
      "href",
      "/runners",
    );
    expect(within(mobileNav).getByRole("link", { name: "项目" })).toHaveAttribute(
      "href",
      "/projects",
    );
    expect(container.querySelector("[style]")).toBeNull();
  });

  it("shows local mode instead of a logout action", async () => {
    server.use(mockMe({ local_mode: true, expires_at_ms: null }), ...mockOverviewResources());
    renderApp("/");

    expect(await screen.findByText("本地模式")).toBeInTheDocument();
    expect(screen.getByText("本地模式 · API 无鉴权")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "退出登录" })).not.toBeInTheDocument();
  });

  it("moves focus to <main> from the skip link", async () => {
    signedIn();
    renderApp("/");

    await screen.findByRole("heading", { level: 1, name: "总览" });
    fireEvent.click(screen.getByRole("link", { name: "跳到主要内容" }));

    expect(screen.getByRole("main")).toHaveFocus();
  });

  it("opens the drawer with focus trapped; Escape and backdrop close it", async () => {
    signedIn();
    const { container } = renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });

    const toggle = screen.getByRole("button", { name: "打开导航菜单" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(toggle);
    const drawer = screen.getByRole("dialog", { name: "导航菜单" });
    const close = screen.getByRole("button", { name: "关闭导航菜单" });

    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(close).toHaveFocus();
    expect(document.body).toHaveClass("nuwa-scroll-lock");
    // The rest of the document is inert while the drawer is open.
    expect(container).toHaveAttribute("inert");
    expect(document.querySelector("[style]")).toBeNull();

    // Tab from the last focusable element wraps to the first (close).
    within(drawer).getByRole("link", { name: "设置" }).focus();
    fireEvent.keyDown(drawer, { key: "Tab" });
    expect(close).toHaveFocus();

    fireEvent.keyDown(drawer, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "导航菜单" })).not.toBeInTheDocument());
    expect(toggle).toHaveFocus();
    expect(document.body).not.toHaveClass("nuwa-scroll-lock");
    expect(container).not.toHaveAttribute("inert");

    fireEvent.click(toggle);
    fireEvent.mouseDown(screen.getByTestId("nav-drawer-backdrop"));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "导航菜单" })).not.toBeInTheDocument());
    expect(toggle).toHaveFocus();
  });

  it("closes the drawer on navigation and restores focus to the menu button", async () => {
    signedIn();
    server.use(mockIssues());
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });

    const toggle = screen.getByRole("button", { name: "打开导航菜单" });
    fireEvent.click(toggle);
    const drawer = screen.getByRole("dialog", { name: "导航菜单" });
    fireEvent.click(within(drawer).getByRole("link", { name: "问题单" }));

    expect(
      await screen.findByRole("heading", { level: 2, name: "问题单" }),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "导航菜单" })).not.toBeInTheDocument(),
    );
    expect(toggle).toHaveFocus();
    expect(toggle).toHaveAttribute("aria-expanded", "false");
  });

  it("toggles the command palette from Ctrl+K and Meta+K", async () => {
    signedIn();
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });
    // The shortcut listener registers before the title effect, so the title
    // proves the shell effects (listener included) have flushed.
    await waitFor(() => expect(document.title).toBe("总览 · 女娲"));

    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    expect(
      await screen.findByRole(
        "dialog",
        { name: "搜索问题单" },
        { timeout: 3_000 },
      ),
    ).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "K", metaKey: true });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );

    fireEvent.keyDown(document, { key: "K", metaKey: true });
    expect(
      await screen.findByRole(
        "dialog",
        { name: "搜索问题单" },
        { timeout: 3_000 },
      ),
    ).toBeInTheDocument();
  });

  it("opens the command palette from the topbar search button", async () => {
    signedIn();
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });
    await waitFor(() => expect(document.title).toBe("总览 · 女娲"));

    const trigger = desktopSearchTrigger();
    expect(trigger).toBeEnabled();
    expect(trigger).toHaveTextContent("搜索问题单…");
    expect(trigger).toHaveTextContent("⌘K");

    fireEvent.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "搜索问题单" });
    const input = within(dialog).getByRole("searchbox");
    await waitFor(() => expect(input).toHaveFocus());
    expect(trigger).toHaveAttribute("aria-expanded", "true");

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("opens the palette from the mobile search icon and restores focus to it", async () => {
    signedIn();
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });
    await waitFor(() => expect(document.title).toBe("总览 · 女娲"));

    const trigger = mobileSearchTrigger();
    expect(trigger).toHaveClass("md:hidden");
    expect(trigger).toHaveClass("size-11");
    expect(trigger).toHaveAttribute("aria-haspopup", "dialog");
    expect(trigger).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "搜索问题单" });
    const input = within(dialog).getByRole("searchbox");
    await waitFor(() => expect(input).toHaveFocus());
    expect(trigger).toHaveAttribute("aria-expanded", "true");

    fireEvent.keyDown(input, { key: "Escape" });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("closes the drawer when the palette opens and keeps the page locked", async () => {
    signedIn();
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });
    await waitFor(() => expect(document.title).toBe("总览 · 女娲"));

    fireEvent.click(screen.getByRole("button", { name: "打开导航菜单" }));
    expect(screen.getByRole("dialog", { name: "导航菜单" })).toBeInTheDocument();
    expect(document.body).toHaveClass("nuwa-scroll-lock");

    fireEvent.keyDown(document, { key: "k", ctrlKey: true });

    const dialog = await screen.findByRole("dialog", { name: "搜索问题单" });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "导航菜单" }),
      ).not.toBeInTheDocument(),
    );
    // The palette takes the page lock over from the drawer instead of both
    // portals holding (and later releasing) it independently.
    expect(document.body).toHaveClass("nuwa-scroll-lock");
    await waitFor(() => expect(within(dialog).getByRole("searchbox")).toHaveFocus());
  });

  it("opens the palette with Ctrl+K from a focused input and returns focus to it", async () => {
    signedIn();
    server.use(mockIssues());
    renderApp("/issues");
    await screen.findByRole("heading", { level: 1, name: "问题单" });
    await waitFor(() => expect(document.title).toBe("问题单 · 女娲"));

    const search = screen.getByRole("searchbox", { name: "搜索" });
    search.focus();
    expect(search).toHaveFocus();

    fireEvent.keyDown(search, { key: "k", ctrlKey: true });

    const dialog = await screen.findByRole("dialog", { name: "搜索问题单" });
    await waitFor(() => expect(within(dialog).getByRole("searchbox")).toHaveFocus());

    fireEvent.keyDown(within(dialog).getByRole("searchbox"), { key: "Escape" });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );
    await waitFor(() => expect(search).toHaveFocus());
  });

  it("shows an offline banner when the transport fails and clears it on retry", async () => {
    // A real transport outage fails every request; if the sibling trends/events
    // queries were left succeeding they could clear the banner before the
    // assertion and make the test order-sensitive.
    const unreachable = () => HttpResponse.error();
    server.use(
      mockMe(),
      http.get("/api/v1/overview", unreachable),
      http.get("/api/v1/metrics/trends", unreachable),
      http.get("/api/v1/events", unreachable),
      http.get("/api/v1/projects", unreachable),
    );

    renderApp("/");

    expect(await screen.findByText(/无法连接服务器。/)).toBeInTheDocument();
    expect(await screen.findByText("serve 不可达")).toBeInTheDocument();
    const offline = screen.getByText(/无法连接服务器。/).closest<HTMLElement>('[role="alert"]');
    if (!offline) {
      throw new Error("offline banner is not an alert");
    }
    // The retry button lives outside the alert live region.
    expect(within(offline).queryByRole("button")).not.toBeInTheDocument();

    server.use(...mockOverviewResources());
    fireEvent.click(screen.getByRole("button", { name: "立即重试" }));

    await waitFor(() =>
      expect(screen.queryByText(/无法连接服务器。/)).not.toBeInTheDocument(),
    );
    expect(await screen.findByText("serve 正常")).toBeInTheDocument();
  });

  it("closes the drawer when the viewport crosses the desktop breakpoint", async () => {
    signedIn();

    type ChangeListener = (event: MediaQueryListEvent) => void;
    const changeListeners = new Set<ChangeListener>();
    const desktopQuery = {
      matches: false,
      media: "(min-width: 1024px)",
      onchange: null,
      addEventListener: vi.fn((type: string, listener: ChangeListener) => {
        if (type === "change") {
          changeListeners.add(listener);
        }
      }),
      removeEventListener: vi.fn((type: string, listener: ChangeListener) => {
        if (type === "change") {
          changeListeners.delete(listener);
        }
      }),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(() => true),
    };
    // jsdom has no matchMedia and theme initialisation also asks for
    // `(prefers-color-scheme: light)`, so answer every query with the same list.
    vi.stubGlobal("matchMedia", vi.fn(() => desktopQuery));

    try {
      const { container } = renderApp("/");
      await screen.findByRole("heading", { level: 1, name: "总览" });
      await screen.findByText("暂无活动记录");
      await screen.findByText("暂无趋势数据");

      const toggle = screen.getByRole("button", { name: "打开导航菜单" });
      fireEvent.click(toggle);

      expect(screen.getByRole("dialog", { name: "导航菜单" })).toBeInTheDocument();
      expect(document.body).toHaveClass("nuwa-scroll-lock");
      expect(container).toHaveAttribute("inert");
      expect(changeListeners.size).toBe(1);

      desktopQuery.matches = true;
      act(() => {
        for (const listener of changeListeners) {
          listener({ matches: true } as MediaQueryListEvent);
        }
      });

      await waitFor(() =>
        expect(screen.queryByRole("dialog", { name: "导航菜单" })).not.toBeInTheDocument(),
      );
      expect(document.body).not.toHaveClass("nuwa-scroll-lock");
      expect(container).not.toHaveAttribute("inert");
      expect(toggle).toHaveAttribute("aria-expanded", "false");
      expect(desktopQuery.removeEventListener).toHaveBeenCalledWith(
        "change",
        expect.any(Function),
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
