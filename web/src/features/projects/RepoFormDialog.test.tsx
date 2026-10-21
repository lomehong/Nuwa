import { QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";

import { ToastProvider } from "../../components/ui";
import { makeRepo, server } from "../../test/msw";
import { createTestQueryClient } from "../../test/render";
import { RepoFormDialog } from "./RepoFormDialog";
import type { RepoDto } from "./registry";

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
