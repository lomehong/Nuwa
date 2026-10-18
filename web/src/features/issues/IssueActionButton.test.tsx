import { QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ToastProvider, type IssueState } from "../../components/ui";
import { server } from "../../test/msw";
import { createTestQueryClient } from "../../test/render";
import { IssueActionButton } from "./IssueActionButton";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
});

function renderAction(props: { state: IssueState; stuck?: boolean; fingerprint?: string }) {
  const queryClient = createTestQueryClient();
  const invalidate = vi.spyOn(queryClient, "invalidateQueries");
  render(
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <IssueActionButton
          fingerprint={props.fingerprint ?? "fp-1"}
          state={props.state}
          stuck={props.stuck ?? false}
        />
      </ToastProvider>
    </QueryClientProvider>,
  );
  return { queryClient, invalidate };
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

    fireEvent.click(screen.getByRole("button", { name: "重新打开" }));

    await waitFor(() => expect(posted).toBe("fp-1:csrf-token"));
    expect(body).toEqual({});
    expect(await screen.findByText("已重新入队")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    const keys = invalidate.mock.calls.map(([filters]) => filters?.queryKey);
    expect(keys).toEqual(
      expect.arrayContaining([["issue", "fp-1"], ["overview"], ["issues"]]),
    );
  });

  it("asks for confirmation before requeueing a stuck issue", async () => {
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

    fireEvent.click(screen.getByRole("button", { name: "重排" }));
    expect(screen.getByRole("dialog", { name: "重排卡单问题？" })).toBeInTheDocument();
    expect(posted).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "确认重排" }));

    await waitFor(() => expect(posted).toBe(true));
    expect(body).toEqual({});
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
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
    expect(screen.queryByRole("button", { name: "重排" })).not.toBeInTheDocument();
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

    fireEvent.click(screen.getByRole("button", { name: "重新打开" }));

    expect(await screen.findByText("重新打开失败")).toBeInTheDocument();
    expect(
      screen.getByText("state is merged（state：expected escalated or rejected）"),
    ).toBeInTheDocument();
  });
});
