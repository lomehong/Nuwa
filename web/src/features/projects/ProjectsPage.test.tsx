import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";

import {
  makeProject,
  makeRepo,
  mockEvents,
  mockMe,
  mockOverview,
  mockProjects,
  mockRepos,
  mockTrends,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";
import type { RepoDto } from "./registry";
import type { ProjectSummary } from "./types";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
});

function signedIn(repos: RepoDto[] = [], projects: ProjectSummary[] = []) {
  server.use(
    mockMe(),
    mockOverview(),
    mockTrends(),
    mockEvents(),
    mockProjects(projects),
    mockRepos(repos),
  );
}

describe("ProjectsPage registry", () => {
  it("registers a repository and shows it waiting for its first heartbeat", async () => {
    const repo = makeRepo({
      repo: "example.com/quiet",
      name: "Quiet",
      url: "https://example.com/quiet",
      description: "quiet service",
    });
    let created = false;
    let body: unknown;
    let csrf: string | null = null;
    let listCalls = 0;
    signedIn();
    server.use(
      http.get("/api/v1/repos", () => {
        listCalls += 1;
        return HttpResponse.json({ data: created ? [repo] : [] });
      }),
      http.post("/api/v1/repos", async ({ request }) => {
        csrf = request.headers.get("x-nuwa-csrf");
        body = await request.json();
        created = true;
        return HttpResponse.json({ data: repo }, { status: 201 });
      }),
    );
    renderApp("/projects");

    fireEvent.click(await screen.findByRole("button", { name: "登记仓库" }));
    const dialog = await screen.findByRole("dialog", { name: "登记仓库" });
    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "Quiet" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库标识"), {
      target: { value: "example.com/quiet" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库地址"), {
      target: { value: "https://example.com/quiet" },
    });
    fireEvent.change(within(dialog).getByLabelText("描述（可选）"), {
      target: { value: "quiet service" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "登记" }));

    await waitFor(() =>
      expect(body).toEqual({
        repo: "example.com/quiet",
        name: "Quiet",
        url: "https://example.com/quiet",
        description: "quiet service",
      }),
    );
    expect(csrf).toBe("csrf-token");
    expect(await screen.findByText("仓库已添加")).toBeInTheDocument();
    expect(await screen.findByText("已登记 · 等待首个心跳")).toBeInTheDocument();
    expect(screen.getByText("example.com/quiet")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "编辑登记：Quiet" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "取消登记：Quiet" })).toBeInTheDocument();
    expect(listCalls).toBeGreaterThan(1);
  });

  it("edits a registry-only registration", async () => {
    const repo = makeRepo({
      repo: "example.com/quiet",
      name: "Quiet",
      url: "https://example.com/quiet",
    });
    let putUrl = "";
    let body: unknown;
    signedIn([repo]);
    server.use(
      http.put("/api/v1/repos/:repo", async ({ request }) => {
        putUrl = new URL(request.url).pathname;
        body = await request.json();
        return HttpResponse.json({ data: { ...repo, name: "Quiet Console" } });
      }),
    );
    renderApp("/projects");

    fireEvent.click(await screen.findByRole("button", { name: "编辑登记：Quiet" }));
    const dialog = await screen.findByRole("dialog", { name: "编辑登记" });
    expect(within(dialog).getByText("example.com/quiet")).toBeInTheDocument();
    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "Quiet Console" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(body).toEqual({
        name: "Quiet Console",
        url: "https://example.com/quiet",
        description: "",
      }),
    );
    expect(putUrl).toBe("/api/v1/repos/example.com%2Fquiet");
    expect(await screen.findByText("仓库已更新")).toBeInTheDocument();
  });

  it("unregisters only after confirmation and leaves the repo alone", async () => {
    const repo = makeRepo({ repo: "example.com/quiet", name: "Quiet" });
    const deletes: string[] = [];
    let removed = false;
    signedIn([repo]);
    server.use(
      http.get("/api/v1/repos", () =>
        HttpResponse.json({ data: removed ? [] : [repo] }),
      ),
      http.delete("/api/v1/repos/:repo", ({ request }) => {
        deletes.push(new URL(request.url).pathname);
        removed = true;
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderApp("/projects");

    fireEvent.click(await screen.findByRole("button", { name: "取消登记：Quiet" }));
    const dialog = await screen.findByRole("dialog", { name: "取消这个仓库的登记？" });
    expect(within(dialog).getByText(/不会停止 runner/)).toBeInTheDocument();
    expect(within(dialog).getByText(/nuwa\.repo\.json/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "保留登记" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "取消这个仓库的登记？" }),
      ).not.toBeInTheDocument(),
    );
    expect(deletes).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "取消登记：Quiet" }));
    const confirm = await screen.findByRole("dialog", { name: "取消这个仓库的登记？" });
    fireEvent.click(within(confirm).getByRole("button", { name: "取消登记" }));
    await waitFor(() =>
      expect(deletes).toEqual(["/api/v1/repos/example.com%2Fquiet"]),
    );
    expect(await screen.findByText("已取消登记")).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByText("已登记 · 等待首个心跳")).not.toBeInTheDocument(),
    );
  });

  it("does not list a registered repository that already has a project row", async () => {
    const repo = makeRepo({ repo: "example.com/app", name: "App" });
    const project = makeProject({ repo: "example.com/app", name: "App" });
    signedIn([repo], [project]);
    renderApp("/projects");

    expect(await screen.findByRole("heading", { level: 2, name: "项目台账" })).toBeInTheDocument();
    expect(screen.queryByText("已登记 · 等待首个心跳")).not.toBeInTheDocument();
  });
});
