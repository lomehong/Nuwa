import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import {
  makeMemory,
  makeMemoryList,
  mockMe,
  mockOverview,
  mockProjectMemory,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

const REPO = "example.com/acme/app";
const ROUTE = `/projects/${encodeURIComponent(REPO)}/memory`;

function fixture(disabled = false) {
  return makeMemoryList({
    data: [
      makeMemory({ disabled }),
      makeMemory({
        fingerprint: "fp-other",
        title: "TypeError in handler",
        state: "pr_open",
        commit_sha: "fedcba9876543210",
        pr_url: null,
        root_cause: "ROOT CAUSE: missing null guard",
        change_stat: "handler.ts | 4 ++--",
        files: 2,
      }),
    ],
    total: 2,
  });
}

describe("项目修复记忆", () => {
  it("lists provenance and disables a memory through the API", async () => {
    let patched: string | null = null;
    server.use(
      mockMe(),
      mockOverview(),
      mockProjectMemory(fixture()),
      http.patch(/\/api\/v1\/projects\/.*\/memory\/.*$/, async ({ request }) => {
        patched = await request.text();
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderApp(ROUTE);

    expect(await screen.findByRole("heading", { level: 2, name: "修复记忆" })).toBeInTheDocument();
    expect(await screen.findByText("panic in Average")).toBeInTheDocument();
    expect(screen.getByText(/ROOT CAUSE: loop bound/)).toBeInTheDocument();
    expect(screen.getByText("commit 0123456789ab")).toBeInTheDocument();
    expect(screen.getAllByRole("link", { name: /打开 PR/ }).length).toBeGreaterThan(0);

    fireEvent.click(screen.getAllByRole("button", { name: "停用" })[0]);
    await waitFor(() => expect(patched).toContain('"disabled":true'));
  });

  it("requires confirmation before deleting and reports disabled state", async () => {
    let deleted = false;
    server.use(
      mockMe(),
      mockOverview(),
      mockProjectMemory(fixture(true)),
      http.delete(/\/api\/v1\/projects\/.*\/memory\/.*$/, () => {
        deleted = true;
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderApp(ROUTE);

    await screen.findByText("panic in Average");
    expect(screen.getAllByText("已停用").length).toBeGreaterThan(0);
    expect(screen.getAllByRole("button", { name: "启用" }).length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: "删除记忆：fp-memory" }));
    const dialog = screen.getByRole("dialog", { name: "删除这条修复记忆？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));
    await waitFor(() => expect(deleted).toBe(true));
  });
});
