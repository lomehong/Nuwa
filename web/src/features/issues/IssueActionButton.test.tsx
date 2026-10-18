import { QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ToastProvider, type IssueState } from "../../components/ui";
import { server } from "../../test/msw";
import { createTestQueryClient } from "../../test/render";
import { IssueActionButton } from "./IssueActionButton";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
});

interface ActionProps {
  state: IssueState;
  stuck?: boolean;
  fingerprint?: string;
}

function renderAction(props: ActionProps) {
  const queryClient = createTestQueryClient();
  const invalidate = vi.spyOn(queryClient, "invalidateQueries");
  const tree = (next: ActionProps) => (
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <IssueActionButton
          fingerprint={next.fingerprint ?? "fp-1"}
          state={next.state}
          stuck={next.stuck ?? false}
        />
      </ToastProvider>
    </QueryClientProvider>
  );
  const view = render(tree(props));
  return {
    queryClient,
    invalidate,
    rerender: (next: ActionProps) => view.rerender(tree(next)),
  };
}

describe("IssueActionButton", () => {
  it("reopens immediately with CSRF, a toast and invalidated views", async () => {
    let posted: string | null = null;
    let body: unknown;
    server.use(
      http.post("/api/v1/issues/:fp/reopen", async ({ request, params }) => {
        posted = `${String(params.fp)}:${request.headers.get("x-nuwa-csrf")}`;
        body = await request.json();
        return new HttpResponse(null, { status: 204 });
      }),
    );

    const { invalidate } = renderAction({ state: "rejected" });

    fireEvent.click(screen.getByRole("button", { name: "重新打开：fp-1" }));

    await waitFor(() => expect(posted).toBe("fp-1:csrf-token"));
    expect(body).toEqual({});
    expect(await screen.findByText("已重新入队")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    const keys = invalidate.mock.calls.map(([filters]) => filters?.queryKey);
    expect(keys).toEqual(
      expect.arrayContaining([["issue", "fp-1"], ["overview"], ["issues"]]),
    );
  });

  it("names reopen with the visible label and short fingerprint", () => {
    const fingerprint = "fp-1234567890abcdef";

    renderAction({ state: "rejected", fingerprint });
    expect(
      screen.getByRole("button", { name: "重新打开：fp-1234567…" }),
    ).toBeInTheDocument();
  });

  it("names requeue with the visible label and short fingerprint", () => {
    renderAction({
      state: "fixing",
      stuck: true,
      fingerprint: "fp-1234567890abcdef",
    });

    expect(screen.getByRole("button", { name: "重排：fp-1234567…" })).toBeInTheDocument();
  });

  it("asks for confirmation before requeueing and toasts the result", async () => {
    let posted = false;
    let body: unknown;
    server.use(
      http.post("/api/v1/issues/:fp/requeue", async ({ request }) => {
        posted = true;
        body = await request.json();
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderAction({ state: "fixing", stuck: true });

    fireEvent.click(screen.getByRole("button", { name: "重排：fp-1" }));
    expect(screen.getByRole("dialog", { name: "重排卡单问题？" })).toBeInTheDocument();
    expect(posted).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "确认重排" }));

    await waitFor(() => expect(posted).toBe(true));
    expect(body).toEqual({});
    expect(await screen.findByText("已重排")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("cancels the requeue dialog without posting", () => {
    let posts = 0;
    server.use(
      http.post("/api/v1/issues/:fp/requeue", () => {
        posts += 1;
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderAction({ state: "fixing", stuck: true });

    fireEvent.click(screen.getByRole("button", { name: "重排：fp-1" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(posts).toBe(0);
  });

  it("keeps the requeue dialog closed when the issue leaves and re-enters stuck", () => {
    const { rerender } = renderAction({ state: "fixing", stuck: true });

    fireEvent.click(screen.getByRole("button", { name: "重排：fp-1" }));
    expect(screen.getByRole("dialog", { name: "重排卡单问题？" })).toBeInTheDocument();

    // A poll clears `stuck`: the action disappears and the dialog must close.
    rerender({ state: "fixing", stuck: false });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    // A later poll re-adds `stuck`; the stale open flag must not resurrect it.
    rerender({ state: "fixing", stuck: true });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("does not POST twice after a successful write", async () => {
    let posts = 0;
    server.use(
      http.post("/api/v1/issues/:fp/reopen", () => {
        posts += 1;
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderAction({ state: "rejected" });
    const button = screen.getByRole("button", { name: "重新打开：fp-1" });

    fireEvent.click(button);
    await waitFor(() => expect(posts).toBe(1));
    await waitFor(() => expect(button).toBeDisabled());

    fireEvent.click(button);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(posts).toBe(1);
  });

  it("re-enables the action when the row reports a different actionable state", async () => {
    server.use(
      http.post(
        "/api/v1/issues/:fp/reopen",
        () => new HttpResponse(null, { status: 204 }),
      ),
    );

    const { rerender } = renderAction({ state: "rejected" });
    const button = screen.getByRole("button", { name: "重新打开：fp-1" });

    fireEvent.click(button);
    await waitFor(() => expect(button).toBeDisabled());

    // rejected -> escalated still offers reopen, so the same mounted button
    // must become usable again instead of latching disabled forever.
    rerender({ state: "escalated" });
    expect(button).toBeEnabled();
  });

  it("clears the cooldown on its own after 3s", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      server.use(
        http.post(
          "/api/v1/issues/:fp/reopen",
          () => new HttpResponse(null, { status: 204 }),
        ),
      );

      renderAction({ state: "rejected" });
      const button = screen.getByRole("button", { name: "重新打开：fp-1" });

      fireEvent.click(button);
      await waitFor(() => expect(button).toBeDisabled());

      await act(async () => {
        await vi.advanceTimersByTimeAsync(3000);
      });
      expect(button).toBeEnabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it.each(["new", "pr_open", "merged"] as const)(
    "offers no action for %s issues",
    (state) => {
      renderAction({ state });
      expect(screen.queryByRole("button")).not.toBeInTheDocument();
    },
  );

  it.each([
    ["triggered", false],
    ["fixing", false],
  ] as const)("does not requeue a healthy %s issue", (state, stuck) => {
    renderAction({ state, stuck });
    expect(screen.queryByRole("button", { name: /^重排：/ })).not.toBeInTheDocument();
  });

  it("surfaces a 409 with its field detail in a danger toast", async () => {
    server.use(
      http.post("/api/v1/issues/:fp/reopen", () =>
        HttpResponse.json(
          {
            error: {
              code: "invalid_state",
              message: "state is merged",
              fields: { state: "expected escalated or rejected" },
            },
          },
          { status: 409 },
        ),
      ),
    );

    renderAction({ state: "escalated" });

    fireEvent.click(screen.getByRole("button", { name: "重新打开：fp-1" }));

    expect(await screen.findByText("重新打开失败")).toBeInTheDocument();
    expect(
      screen.getByText("state is merged（state：expected escalated or rejected）"),
    ).toBeInTheDocument();
  });

  it("toasts the status fallback when the error body is not JSON", async () => {
    server.use(
      http.post(
        "/api/v1/issues/:fp/reopen",
        () =>
          new HttpResponse("<html>not found</html>", {
            status: 404,
            headers: { "Content-Type": "text/html" },
          }),
      ),
    );

    renderAction({ state: "rejected" });

    fireEvent.click(screen.getByRole("button", { name: "重新打开：fp-1" }));

    expect(await screen.findByText("重新打开失败")).toBeInTheDocument();
    expect(screen.getByText("资源不存在")).toBeInTheDocument();
  });
});
