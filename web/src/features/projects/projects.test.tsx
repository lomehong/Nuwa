import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  makeIssue,
  makeProject,
  makeRunner,
  mockEvents,
  mockIssues,
  mockMe,
  mockOverview,
  mockProjects,
  mockRunners,
  mockTrends,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

const INGEST = "example.com/acme/ingest";

function projects() {
  return [
    makeProject({
      repo: INGEST,
      name: "Ingest",
      url: "https://example.com/acme/ingest",
      attention: 2,
      queued: 3,
      running: 1,
      pr_open: 1,
      escalated: 1,
      rejected: 1,
      runners: 2,
      runners_online: 1,
      prs_today: 3,
      pr_limit: 5,
      ci_green: 4,
      ci_red: 1,
      reports_24h: 12,
      tokens_24h: 66_200,
      worst_severity: "critical",
      last_signal_ms: Date.now() - 60_000,
    }),
    makeProject({
      repo: "example.com/acme/web",
      name: "Web",
      attention: 0,
      queued: 1,
    }),
  ];
}

function signedIn() {
  server.use(
    mockMe(),
    mockOverview(),
    mockTrends(),
    mockEvents(),
    mockIssues(),
    mockRunners(),
    mockProjects(projects()),
  );
}

describe("项目台账", () => {
  it("lists every project with the numbers that decide whether to open it", async () => {
    signedIn();
    renderApp("/projects");

    expect(
      await screen.findByRole("heading", { level: 2, name: "项目台账" }),
    ).toBeInTheDocument();

    const main = within(screen.getByRole("main"));
    // The table and the mobile list both carry the project link.
    const link = (await main.findAllByRole("link", { name: "Ingest" }))[0];
    expect(link).toHaveAttribute("href", `/projects/${encodeURIComponent(INGEST)}`);

    // 需处理 / 队列 / 运行中 / PR 待合并 are the triage columns.
    const row = link.closest("tr");
    expect(row).not.toBeNull();
    expect(row).toHaveTextContent("2");
    expect(row).toHaveTextContent("3");
    expect(row).toHaveTextContent("1");
    // 运行器 在线/总数 and 今日 PR.
    expect(row).toHaveTextContent("1/2");
    expect(row).toHaveTextContent("3");
    // CI 绿/红.
    expect(row).toHaveTextContent("4");
  });

  it("puts the projects in the navigation rail and marks the current one", async () => {
    signedIn();
    renderApp(`/projects/${encodeURIComponent(INGEST)}`);

    const rail = within(
      (await screen.findAllByRole("navigation")).find(
        (nav) => nav.getAttribute("aria-label") === "主导航",
      )!,
    );
    const nameplate = (await rail.findAllByRole("link", { name: /Ingest/ }))[0];
    expect(nameplate).toHaveAttribute("href", `/projects/${encodeURIComponent(INGEST)}`);
    // The active project carries the ember rule.
    expect(nameplate.className).toContain("nameplate--active");
  });
});

describe("项目工位", () => {
  it("shows the project's readings, its backlog and its runner", async () => {
    server.use(
      mockMe(),
      mockOverview({
        attention: [
          {
            fingerprint: "fp-escalated",
            title: "Ingest panics on empty batch",
            state: "escalated",
            severity: "critical",
            repo: INGEST,
            last_seen_ms: Date.now() - 120_000,
            stuck: false,
            owner_runner_id: "runner-a",
            reason_code: "repair_failed",
            reason_text: "修复 runner 失败，需要复核",
            pr_url: null,
          },
        ],
      }),
      mockTrends(),
      mockEvents(),
      mockIssues({
        data: [
          makeIssue({
            fingerprint: "fp-escalated",
            title: "Ingest panics on empty batch",
            repo: INGEST,
            severity: "critical",
            state: "escalated",
            attempts: 2,
          }),
        ],
      }),
      mockRunners([makeRunner({ id: "runner-1", repo: INGEST })]),
      mockProjects(projects()),
    );
    renderApp(`/projects/${encodeURIComponent(INGEST)}`);

    expect(
      await screen.findByRole("heading", { level: 2, name: "Ingest" }),
    ).toBeInTheDocument();
    const main = within(screen.getByRole("main"));

    // The ledger strip: one reading per column.
    expect(main.getByText("需处理")).toBeInTheDocument();
    expect(main.getByText("队列")).toBeInTheDocument();
    expect(main.getByText("运行中")).toBeInTheDocument();
    expect(main.getByText("tokens 24h")).toBeInTheDocument();

    // The project's own backlog and the runner that serves it.
    expect(main.getAllByText("Ingest panics on empty batch").length).toBeGreaterThan(0);
    expect(main.getByText("修复 runner 失败，需要复核")).toBeInTheDocument();
    expect(main.getByText("runner-1")).toBeInTheDocument();
  });

  it("explains an unknown project instead of rendering an empty shell", async () => {
    signedIn();
    renderApp("/projects/example.com%2Fmissing");

    expect(await screen.findByText("找不到这个项目")).toBeInTheDocument();
  });
});
