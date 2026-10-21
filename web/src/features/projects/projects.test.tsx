import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import {
  makeIssue,
  makeProject,
  makeRepo,
  makeRunner,
  mockEvents,
  mockIssues,
  mockMe,
  mockOverview,
  mockProjects,
  mockRepos,
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

  // WHY: `useParams` already decodes, so a second `decodeURIComponent` on an
  // identity containing a literal `%` used to throw `URIError` and blank the
  // page. The project workstation must render it verbatim.
  it("renders a project whose identity contains a literal percent sign", async () => {
    const percentRepo = "corp/50%done";
    server.use(
      mockMe(),
      mockOverview(),
      mockTrends(),
      mockEvents(),
      mockIssues(),
      mockRunners(),
      mockProjects([makeProject({ repo: percentRepo, name: "Half Done" })]),
    );
    renderApp(`/projects/${encodeURIComponent(percentRepo)}`);

    // The percent-bearing identity resolves to the real project (no throw, no
    // "not found"), and the mono caption shows the exact stored identity.
    expect(
      await screen.findByRole("heading", { level: 2, name: "Half Done" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("找不到这个项目")).not.toBeInTheDocument();
    expect(screen.getAllByText(percentRepo).length).toBeGreaterThan(0);
  });
});

describe("项目登记编辑", () => {
  it("edits the registration from the project page", async () => {
    const repo = makeRepo({
      repo: INGEST,
      name: "Ingest",
      url: "https://example.com/acme/ingest",
    });
    let putUrl = "";
    let body: unknown;
    server.use(
      mockRepos([repo]),
      http.put("/api/v1/repos/:repo", async ({ request }) => {
        putUrl = new URL(request.url).pathname;
        body = await request.json();
        return HttpResponse.json({ data: { ...repo, name: "采集服务" } });
      }),
    );
    signedIn();
    renderApp(`/projects/${encodeURIComponent(INGEST)}`);

    fireEvent.click(await screen.findByRole("button", { name: "编辑登记" }));
    const dialog = await screen.findByRole("dialog", { name: "编辑项目登记" });
    expect(within(dialog).getByLabelText("名称")).toHaveValue("Ingest");
    // The identity is path-owned: shown as text, never as an editable field.
    expect(within(dialog).queryByLabelText("仓库标识")).not.toBeInTheDocument();
    expect(within(dialog).getByText(INGEST)).toBeInTheDocument();

    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "采集服务" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(body).toEqual({
        name: "采集服务",
        url: "https://example.com/acme/ingest",
        description: "",
      }),
    );
    expect(putUrl).toBe(`/api/v1/repos/${encodeURIComponent(INGEST)}`);
  });

  it("registers an unreported project without letting its identity be typed over", async () => {
    let body: unknown;
    server.use(
      mockRepos([]),
      http.post("/api/v1/repos", async ({ request }) => {
        body = await request.json();
        return HttpResponse.json({ data: makeRepo({ repo: INGEST }) });
      }),
    );
    signedIn();
    renderApp(`/projects/${encodeURIComponent(INGEST)}`);

    fireEvent.click(await screen.findByRole("button", { name: "登记此项目" }));
    const dialog = await screen.findByRole("dialog", { name: "登记此项目" });
    // The project already exists (it reported signals): its id is fixed.
    expect(within(dialog).queryByLabelText("仓库标识")).not.toBeInTheDocument();
    expect(within(dialog).getByText(INGEST)).toBeInTheDocument();
    expect(within(dialog).getByLabelText("名称")).toHaveValue("ingest");

    fireEvent.change(within(dialog).getByLabelText("仓库地址"), {
      target: { value: "https://example.com/acme/ingest" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "登记" }));

    await waitFor(() =>
      expect(body).toEqual({
        repo: INGEST,
        name: "ingest",
        url: "https://example.com/acme/ingest",
        description: "",
      }),
    );
  });

  it("unregisters only after confirmation and leaves the repo alone", async () => {
    const repo = makeRepo({ repo: INGEST, name: "Ingest" });
    const deletes: string[] = [];
    server.use(
      mockRepos([repo]),
      http.delete("/api/v1/repos/:repo", ({ request }) => {
        deletes.push(new URL(request.url).pathname);
        return new HttpResponse(null, { status: 204 });
      }),
    );
    signedIn();
    renderApp(`/projects/${encodeURIComponent(INGEST)}`);

    fireEvent.click(await screen.findByRole("button", { name: "取消登记" }));
    const dialog = await screen.findByRole("dialog", { name: "取消这个项目的登记？" });
    expect(within(dialog).getByText(/不会停止 runner/)).toBeInTheDocument();
    expect(within(dialog).getByText(/nuwa\.repo\.json/)).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "保留" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "取消这个项目的登记？" }),
      ).not.toBeInTheDocument(),
    );
    expect(deletes).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "取消登记" }));
    const confirm = await screen.findByRole("dialog", { name: "取消这个项目的登记？" });
    fireEvent.click(within(confirm).getByRole("button", { name: "确认取消登记" }));
    await waitFor(() =>
      expect(deletes).toEqual([`/api/v1/repos/${encodeURIComponent(INGEST)}`]),
    );
    expect(await screen.findByText("已取消登记")).toBeInTheDocument();
  });

  it("shows the target repo's own content as read-only", async () => {
    signedIn();
    server.use(
      mockRunners([
        makeRunner({
          id: "runner-1",
          repo: INGEST,
          effective: {
            interval_secs: 30,
            ci_timeout_secs: 600,
            max_turns: 30,
            max_tokens: 1_500_000,
            failing_check: "go test ./...",
            reproduce_check: ".nuwa/repro.sh",
            forbidden_paths_count: 3,
            max_attempts: 3,
          },
        }),
      ]),
    );
    renderApp(`/projects/${encodeURIComponent(INGEST)}`);

    const main = within(await screen.findByRole("main"));
    expect(await main.findByText("仓库内容")).toBeInTheDocument();
    expect(main.getByText("go test ./...")).toBeInTheDocument();
    expect(main.getByText(".nuwa/repro.sh")).toBeInTheDocument();
    expect(main.getByText("3 条")).toBeInTheDocument();
    // The content comes from the target repo; the console never edits it.
    expect(main.getByText(/来自目标仓的 nuwa\.repo\.json/)).toBeInTheDocument();
  });
});
