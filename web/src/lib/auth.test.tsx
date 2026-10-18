import { QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import { createTestQueryClient } from "../test/render";
import { mockMe, mockUnauthorizedMe, server } from "../test/msw";
import { apiFetch } from "./api";
import { AuthProvider, useAuth } from "./auth";

function AuthProbe() {
  const auth = useAuth();
  return (
    <div>
      <span data-testid="status">{auth.status}</span>
      <span data-testid="local-mode">{String(auth.localMode)}</span>
      <button type="button" onClick={() => void auth.login("ui-token").catch(() => undefined)}>
        login
      </button>
      <button type="button" onClick={() => void auth.logout().catch(() => undefined)}>
        logout
      </button>
      <button type="button" onClick={() => auth.retry()}>
        retry
      </button>
    </div>
  );
}

function renderAuth() {
  const queryClient = createTestQueryClient();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <AuthProbe />
      </AuthProvider>
    </QueryClientProvider>,
  );
  return { ...result, queryClient };
}

describe("AuthProvider", () => {
  it("treats a 401 from /auth/me as not logged in", async () => {
    server.use(mockUnauthorizedMe());
    renderAuth();

    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("unauthenticated"),
    );
    expect(screen.getByTestId("local-mode")).toHaveTextContent("false");
  });

  it("stays authenticated in local mode", async () => {
    server.use(mockMe({ local_mode: true, expires_at_ms: null }));
    renderAuth();

    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("authenticated"),
    );
    expect(screen.getByTestId("local-mode")).toHaveTextContent("true");
  });

  it("logs in by posting the token and flipping the auth state", async () => {
    server.use(mockUnauthorizedMe());
    let received: unknown = null;
    server.use(
      http.post("/api/v1/auth/login", async ({ request }) => {
        received = await request.json();
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderAuth();
    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("unauthenticated"),
    );

    fireEvent.click(screen.getByRole("button", { name: "login" }));

    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("authenticated"),
    );
    expect(received).toEqual({ token: "ui-token" });
  });

  it("logs out with CSRF and returns to the unauthenticated state", async () => {
    server.use(mockMe());
    document.cookie = "nuwa_csrf=logout-csrf; path=/";
    let csrfHeader: string | null = null;
    let method = "";
    server.use(
      http.post("/api/v1/auth/logout", ({ request }) => {
        method = request.method;
        csrfHeader = request.headers.get("x-nuwa-csrf");
        return new HttpResponse(null, { status: 204 });
      }),
    );

    renderAuth();
    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("authenticated"),
    );

    fireEvent.click(screen.getByRole("button", { name: "logout" }));

    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("unauthenticated"),
    );
    expect(method).toBe("POST");
    expect(csrfHeader).toBe("logout-csrf");
    document.cookie = "nuwa_csrf=; Max-Age=0; path=/";
  });

  it("offers a retryable error state when the boot check is not a 401", async () => {
    let fail = true;
    server.use(
      http.get("/api/v1/auth/me", () =>
        fail
          ? HttpResponse.error()
          : HttpResponse.json({
              authenticated: true,
              local_mode: false,
              expires_at_ms: 1_700_000_000_000,
            }),
      ),
    );

    renderAuth();
    await waitFor(() => expect(screen.getByTestId("status")).toHaveTextContent("error"));

    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "retry" }));

    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("authenticated"),
    );
  });

  it("clears cached server state when a background request returns 401", async () => {
    server.use(
      mockMe(),
      http.get("/api/v1/issues", () =>
        HttpResponse.json(
          { error: { code: "unauthorized", message: "session expired" } },
          { status: 401 },
        ),
      ),
    );

    const { queryClient } = renderAuth();
    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("authenticated"),
    );
    queryClient.setQueryData(["probe"], { stale: true });

    await act(async () => {
      await apiFetch("/issues").catch(() => undefined);
    });

    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("unauthenticated"),
    );
    expect(queryClient.getQueryCache().getAll()).toHaveLength(0);
  });

  it("treats a 401 on logout as an already-expired session", async () => {
    server.use(
      mockMe(),
      http.post("/api/v1/auth/logout", () =>
        HttpResponse.json(
          { error: { code: "unauthorized", message: "session expired" } },
          { status: 401 },
        ),
      ),
    );
    document.cookie = "nuwa_csrf=logout-csrf; path=/";

    renderAuth();
    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("authenticated"),
    );

    fireEvent.click(screen.getByRole("button", { name: "logout" }));

    await waitFor(() =>
      expect(screen.getByTestId("status")).toHaveTextContent("unauthenticated"),
    );
    document.cookie = "nuwa_csrf=; Max-Age=0; path=/";
  });
});
