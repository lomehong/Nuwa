import { QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";

import { ToastProvider } from "../../components/ui";
import { makeRepo, server } from "../../test/msw";
import { createTestQueryClient } from "../../test/render";
import { RepoFormDialog, validateRepoForm } from "./RepoFormDialog";
import type { RepoDto } from "./registry";

// Keep these examples in sync with api/repos.rs's URL contract test.
const repoUrlCases: [string, boolean][] = [
  ["http://example.com/app", true],
  ["https://example.com/app", true],
  ["ssh://git@example.com/app.git", true],
  ["git://example.com/app.git", true],
  ["git@example.com:app.git", true],
  ["HTTP://example.com/app", true],
  ["HTTPS://example.com/app", true],
  ["SSH://git@example.com/app.git", true],
  ["GIT://example.com/app.git", true],
  ["example.com/app", true],
  ["../app.git", true],
  ["/srv/git/app.git", true],
  ["example.com/app:mirror", true],
  ["git:app", true],
  ["javascript:alert(1)", false],
  ["JavaScript:alert(1)", false],
  ["data:text/html,test", false],
  ["file:///etc/passwd", false],
  ["ftp://example.com/app", false],
  ["https://example.com/a b", false],
  ["https://example.com/a\tb", false],
  ["https://example.com/a\nb", false],
  ["https://example.com/a\u00a0b", false],
  ["https://example.com/a\u2003b", false],
  ["https://example.com/a\0b", false],
  ["https://example.com/a\u007fb", false],
  ["https://example.com/a\u0085b", false],
  ["https://example.com/a\u009fb", false],
  ["", false],
];

const repoA = makeRepo({
  repo: "example.com/a",
  name: "Name A",
  url: "https://example.com/a",
  description: "desc A",
});
const repoB = makeRepo({
  repo: "example.com/b",
  name: "Name B",
  url: "https://example.com/b",
  description: "desc B",
});

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
});

/**
 * Renders the dialog under the providers its mutation/toast hooks need. The
 * query client is created once and reused across `rerender` so the dialog keeps
 * one instance while its `target` prop is swapped — exactly the reuse a history
 * navigation triggers on a shared route.
 */
function renderEditDialog(initialTarget: RepoDto) {
  const queryClient = createTestQueryClient();
  const element = (target: RepoDto) => (
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <RepoFormDialog open mode="edit" target={target} onClose={() => {}} />
      </ToastProvider>
    </QueryClientProvider>
  );
  const view = render(element(initialTarget));
  return { ...view, retarget: (target: RepoDto) => view.rerender(element(target)) };
}

describe("repository URL contract", () => {
  it.each(repoUrlCases)("validates %j with accepted=%s for create and edit", (url, valid) => {
    for (const identityLocked of [false, true]) {
      const errors = validateRepoForm(
        { repo: "example.com/app", name: "App", url, description: "" },
        identityLocked,
      );
      expect(errors.url === undefined).toBe(valid);
    }
  });

  it.each(["http://example.com/app", "git://example.com/app.git"])(
    "registers %s and submits a name-only edit without changing its URL",
    async (url) => {
      let registered: RepoDto | undefined;
      let edited: unknown;
      server.use(
        http.post("/api/v1/repos", async ({ request }) => {
          const body = await request.json() as Record<string, string>;
          registered = makeRepo(body);
          return HttpResponse.json({ data: registered }, { status: 201 });
        }),
        http.put("/api/v1/repos/:repo", async ({ request }) => {
          edited = { path: new URL(request.url).pathname, body: await request.json() };
          return HttpResponse.json({ data: { ...registered, name: "Renamed App" } });
        }),
      );
      const queryClient = createTestQueryClient();
      const createView = render(
        <QueryClientProvider client={queryClient}>
          <ToastProvider>
            <RepoFormDialog open mode="create" onClose={() => {}} />
          </ToastProvider>
        </QueryClientProvider>,
      );
      const createDialog = screen.getByRole("dialog", { name: "添加仓库" });
      for (const [label, value] of [
        ["名称", "App"],
        ["仓库标识", "example.com/app"],
        ["仓库地址", url],
      ]) {
        fireEvent.change(within(createDialog).getByLabelText(label), { target: { value } });
      }
      fireEvent.click(within(createDialog).getByRole("button", { name: "添加" }));
      await screen.findByText("仓库已添加");
      expect(registered).toMatchObject({ repo: "example.com/app", name: "App", url });
      createView.unmount();
      if (!registered) throw new Error("registration did not reach the API");

      renderEditDialog(registered);
      const editDialog = screen.getByRole("dialog", { name: "编辑仓库" });
      expect(within(editDialog).getByLabelText("仓库地址")).toHaveValue(url);
      fireEvent.change(within(editDialog).getByLabelText("名称"), {
        target: { value: "Renamed App" },
      });
      fireEvent.click(within(editDialog).getByRole("button", { name: "保存" }));
      await screen.findByText("仓库已更新");
      expect(edited).toEqual({
        path: "/api/v1/repos/example.com%2Fapp",
        body: { name: "Renamed App", url, description: "" },
      });
    },
  );
});

describe("RepoFormDialog identity binding", () => {
  it("re-seeds to the new project when target changes while open, never submitting A onto B", async () => {
    let putUrl = "";
    let putBody: unknown;
    server.use(
      http.put("/api/v1/repos/:repo", async ({ request }) => {
        putUrl = new URL(request.url).pathname;
        putBody = await request.json();
        return HttpResponse.json({ data: repoB });
      }),
    );

    const { retarget } = renderEditDialog(repoA);
    const dialog = screen.getByRole("dialog", { name: "编辑仓库" });
    expect(within(dialog).getByLabelText("名称")).toHaveValue("Name A");

    // The operator edits A, then a history navigation re-points the SAME open
    // dialog at project B. Their half-typed edit for A must not follow.
    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "Half-typed A" },
    });
    retarget(repoB);

    // The form re-seeds to B: the field and the shown identity are B's.
    expect(within(dialog).getByLabelText("名称")).toHaveValue("Name B");
    expect(within(dialog).getByLabelText("仓库地址")).toHaveValue("https://example.com/b");
    expect(within(dialog).getByText("example.com/b")).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(putUrl).toBe("/api/v1/repos/example.com%2Fb"));
    // The body carries B's values, not A's (the cross-project write this fixes).
    expect(putBody).toEqual({
      name: "Name B",
      url: "https://example.com/b",
      description: "desc B",
    });
  });

  it("submits the identity it was seeded with for a normal edit", async () => {
    let putUrl = "";
    let putBody: unknown;
    server.use(
      http.put("/api/v1/repos/:repo", async ({ request }) => {
        putUrl = new URL(request.url).pathname;
        putBody = await request.json();
        return HttpResponse.json({ data: repoA });
      }),
    );

    renderEditDialog(repoA);
    const dialog = screen.getByRole("dialog", { name: "编辑仓库" });
    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "Renamed A" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(putUrl).toBe("/api/v1/repos/example.com%2Fa"));
    expect(putBody).toEqual({
      name: "Renamed A",
      url: "https://example.com/a",
      description: "desc A",
    });
  });
});
