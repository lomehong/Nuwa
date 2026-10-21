import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { delay, http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import type { IssueListItem } from "../features/issues/types";
import {
  makeIssue,
  makeIssues,
  makeProject,
  mockEvents,
  mockIssueDetail,
  mockIssues,
  mockMe,
  mockOverview,
  mockOverviewResources,
  mockProjects,
  mockRunners,
  mockTranscript,
  mockTrends,
  server,
} from "../test/msw";
import { renderApp } from "../test/render";

function paletteIssue(): IssueListItem {
  return makeIssue({
    fingerprint: "fp-palette-001",
    title: "Average panics on empty slice",
    language: "go",
    repo: "example.com/app",
    severity: "critical",
    state: "escalated",
    occurrences: 3,
    last_seen_ms: Date.now(),
  });
}

function signedIn() {
  server.use(mockMe(), ...mockOverviewResources());
}

/** Issues handler that records every palette query. */
function paletteIssues(requests: URL[] = []) {
  return http.get("/api/v1/issues", ({ request }) => {
    const url = new URL(request.url);
    requests.push(url);
    return HttpResponse.json(
      makeIssues({
        data: [paletteIssue()],
        meta: { page: 1, page_size: 5, total: 1 },
      }),
    );
  });
}

async function openWithShortcut() {
  await screen.findByRole("heading", { level: 1, name: "总览" });
  // `document.title` is set by an effect that runs after the shell registers
  // its shortcut listener, so it is a reliable "shell effects flushed" barrier.
  await waitFor(() => expect(document.title).toBe("总览 · 女娲"));
  fireEvent.keyDown(document, { key: "k", ctrlKey: true });
  return screen.findByRole("dialog", { name: "搜索问题单" }, { timeout: 3_000 });
}

/** The desktop field shares its accessible name with the mobile icon. */
function desktopSearchTrigger(): HTMLElement {
  const trigger = screen
    .getAllByRole("button", { name: "搜索问题单" })
    .find((button) => button.textContent?.includes("搜索问题单…"));
  if (!trigger) {
    throw new Error("desktop search trigger not found");
  }
  return trigger;
}

describe("CommandPalette", () => {
  it("opens with Ctrl+K, searches after the debounce and navigates to the chosen issue", async () => {
    const requests: URL[] = [];
    signedIn();
    server.use(
      paletteIssues(requests),
      mockIssueDetail({ issue: paletteIssue() }),
      mockTranscript(),
    );
    renderApp("/");

    const dialog = await openWithShortcut();
    const input = within(dialog).getByRole("searchbox", { name: "搜索问题单" });
    await waitFor(() => expect(input).toHaveFocus());
    expect(within(dialog).getByText("输入标题或指纹开始搜索。")).toBeInTheDocument();
    expect(requests).toHaveLength(0);

    fireEvent.change(input, { target: { value: "panic" } });

    const link = await within(dialog).findByRole(
      "link",
      { name: /Average panics on empty slice/ },
      { timeout: 3_000 },
    );
    expect(requests).toHaveLength(1);
    expect(Object.fromEntries(requests[0].searchParams)).toEqual({
      q: "panic",
      page_size: "5",
      sort: "last_seen",
      order: "desc",
    });
    expect(within(link).getByText("严重")).toBeInTheDocument();
    expect(within(link).getByText("已升级")).toBeInTheDocument();
    expect(link).toHaveAttribute("href", "/issues/fp-palette-001");
    expect(within(dialog).getByRole("status")).toHaveTextContent("找到 1 条问题单");

    fireEvent.click(link);

    expect(
      await screen.findByRole(
        "heading",
        { level: 2, name: "Average panics on empty slice" },
        { timeout: 3_000 },
      ),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );
  });

  it("closes on Escape and restores focus to the search button", async () => {
    signedIn();
    renderApp("/");
    await screen.findByRole("heading", { level: 1, name: "总览" });

    const trigger = desktopSearchTrigger();
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

  it("clears the query each time it reopens", async () => {
    const requests: URL[] = [];
    signedIn();
    server.use(paletteIssues(requests));
    renderApp("/");

    let dialog = await openWithShortcut();
    fireEvent.change(within(dialog).getByRole("searchbox"), {
      target: { value: "panic" },
    });
    await within(dialog).findByRole(
      "link",
      { name: /Average panics/ },
      { timeout: 3_000 },
    );

    // A second Ctrl+K toggles the palette closed.
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );

    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    dialog = await screen.findByRole(
      "dialog",
      { name: "搜索问题单" },
      { timeout: 3_000 },
    );
    expect(within(dialog).getByRole("searchbox")).toHaveValue("");
    expect(within(dialog).getByText("输入标题或指纹开始搜索。")).toBeInTheDocument();
    expect(within(dialog).queryByRole("link")).not.toBeInTheDocument();
    expect(requests).toHaveLength(1);
  });

  it("opens the first match with Enter", async () => {
    signedIn();
    server.use(
      paletteIssues(),
      mockIssueDetail({ issue: paletteIssue() }),
      mockTranscript(),
    );
    renderApp("/");

    const dialog = await openWithShortcut();
    const input = within(dialog).getByRole("searchbox", { name: "搜索问题单" });
    fireEvent.change(input, { target: { value: "panic" } });
    await within(dialog).findByRole(
      "link",
      { name: /Average panics/ },
      { timeout: 3_000 },
    );

    fireEvent.keyDown(input, { key: "Enter" });

    expect(
      await screen.findByRole(
        "heading",
        { level: 2, name: "Average panics on empty slice" },
        { timeout: 3_000 },
      ),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );
  });

  it("shows the empty message when nothing matches", async () => {
    signedIn();
    server.use(http.get("/api/v1/issues", () => HttpResponse.json(makeIssues())));
    renderApp("/");

    const dialog = await openWithShortcut();
    fireEvent.change(within(dialog).getByRole("searchbox"), {
      target: { value: "zzz-no-match" },
    });

    await waitFor(() =>
      expect(within(dialog).getByRole("status")).toHaveTextContent(
        "没有匹配的问题单",
      ),
    );
    expect(within(dialog).getByRole("status")).toHaveClass("sr-only");
  });

  it("keeps the palette open for a modified result click", async () => {
    signedIn();
    server.use(paletteIssues());
    renderApp("/");

    const dialog = await openWithShortcut();
    const input = within(dialog).getByRole("searchbox", { name: "搜索问题单" });
    fireEvent.change(input, { target: { value: "panic" } });
    const link = await within(dialog).findByRole(
      "link",
      { name: /Average panics/ },
      { timeout: 3_000 },
    );

    // Ctrl+click means "open in a new tab". jsdom would try to navigate the
    // current document, so stop the default action after React saw the event.
    const stopNavigation = (event: Event) => event.preventDefault();
    document.addEventListener("click", stopNavigation);
    try {
      fireEvent.click(link, { button: 0, ctrlKey: true });
    } finally {
      document.removeEventListener("click", stopNavigation);
    }

    expect(screen.getByRole("dialog", { name: "搜索问题单" })).toBeInTheDocument();
    expect(input).toHaveValue("panic");
  });

  it("surfaces request errors outside the retry control", async () => {
    signedIn();
    server.use(
      http.get("/api/v1/issues", () =>
        HttpResponse.json(
          { error: { code: "internal", message: "store read failed" } },
          { status: 500 },
        ),
      ),
    );
    renderApp("/");

    const dialog = await openWithShortcut();
    fireEvent.change(within(dialog).getByRole("searchbox"), {
      target: { value: "panic" },
    });

    const alert = await within(dialog).findByRole("alert", {}, { timeout: 3_000 });
    expect(alert).toHaveTextContent("搜索失败");
    expect(alert).toHaveTextContent("store read failed");
    expect(within(alert).queryByRole("button")).not.toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("shows a loading skeleton and closes on backdrop click", async () => {
    signedIn();
    server.use(
      http.get("/api/v1/issues", async () => {
        await delay("infinite");
        return HttpResponse.json(makeIssues());
      }),
    );
    renderApp("/");

    const dialog = await openWithShortcut();
    fireEvent.change(within(dialog).getByRole("searchbox"), {
      target: { value: "panic" },
    });
    expect(within(dialog).getByText("正在搜索问题单")).toBeInTheDocument();

    fireEvent.mouseDown(screen.getByTestId("command-palette-backdrop"));
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "搜索问题单" }),
      ).not.toBeInTheDocument(),
    );
  });

  // WHY: a query that matches ONLY projects must still render the project rows.
  // The old empty-state check looked at issues alone, so the project links were
  // hidden (and Enter could activate an invisible row).
  it("renders a clickable project row when only projects match", async () => {
    const project = makeProject({
      repo: "example.com/acme/ingest",
      name: "Ingest Service",
    });
    server.use(
      mockMe(),
      mockOverview(),
      mockTrends(),
      mockEvents(),
      mockProjects([project]),
      mockRunners(),
      mockIssues(),
    );
    renderApp("/");

    const dialog = await openWithShortcut();
    const input = within(dialog).getByRole("searchbox", { name: "搜索问题单" });
    fireEvent.change(input, { target: { value: "ingest" } });

    const projectLink = await within(dialog).findByRole(
      "link",
      { name: /Ingest Service/ },
      { timeout: 3_000 },
    );
    expect(projectLink).toHaveAttribute(
      "href",
      "/projects/example.com%2Facme%2Fingest",
    );
    // The "no issues" empty body must not render while a project row is shown.
    expect(
      within(dialog).queryByText("没有匹配的问题单。"),
    ).not.toBeInTheDocument();
    expect(within(dialog).getByRole("status")).toHaveTextContent("找到 1 个项目");

    fireEvent.click(projectLink);

    expect(
      await screen.findByRole(
        "heading",
        { level: 2, name: "Ingest Service" },
        { timeout: 3_000 },
      ),
    ).toBeInTheDocument();
  });
});
