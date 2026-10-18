import { http, HttpResponse } from "msw";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { server } from "../test/msw";
import { ApiError, apiFetch, setUnauthorizedHandler } from "./api";

function clearCsrfCookie() {
  document.cookie = "nuwa_csrf=; Max-Age=0; path=/";
}

let restoreHandler: (() => void) | null = null;

beforeEach(() => {
  clearCsrfCookie();
});

afterEach(() => {
  restoreHandler?.();
  restoreHandler = null;
  clearCsrfCookie();
});

describe("apiFetch", () => {
  it("sends same-origin credentials and parses a JSON response", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    server.use(http.get("/api/v1/overview", () => HttpResponse.json({ ok: true })));

    await expect(apiFetch<{ ok: boolean }>("/overview")).resolves.toEqual({ ok: true });
    expect(fetchSpy).toHaveBeenCalledWith(
      "/api/v1/overview",
      expect.objectContaining({ credentials: "include" }),
    );
  });

  it("echoes the nuwa_csrf cookie in X-NUWA-CSRF on mutating requests", async () => {
    document.cookie = "nuwa_csrf=csrf-token-123; path=/";
    let header: string | null = null;
    let contentType: string | null = null;
    let body: unknown;

    server.use(
      http.post("/api/v1/auth/login", async ({ request }) => {
        header = request.headers.get("x-nuwa-csrf");
        contentType = request.headers.get("content-type");
        body = await request.json();
        return new HttpResponse(null, { status: 204 });
      }),
    );

    await expect(
      apiFetch("/auth/login", { method: "POST", body: JSON.stringify({ token: "secret" }) }),
    ).resolves.toBeUndefined();

    expect(header).toBe("csrf-token-123");
    expect(contentType).toBe("application/json");
    expect(body).toEqual({ token: "secret" });
  });

  it("does not send the CSRF header on GET", async () => {
    document.cookie = "nuwa_csrf=csrf-token-123; path=/";
    let header: string | null = "unset";
    server.use(
      http.get("/api/v1/overview", ({ request }) => {
        header = request.headers.get("x-nuwa-csrf");
        return HttpResponse.json({});
      }),
    );

    await apiFetch("/overview");
    expect(header).toBeNull();
  });

  it("parses the error envelope into an ApiError with fields", async () => {
    server.use(
      http.post("/api/v1/issues/abc/reopen", () =>
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

    const error = await apiFetch("/issues/abc/reopen", { method: "POST" }).catch(
      (reason: unknown) => reason,
    );

    expect(error).toBeInstanceOf(ApiError);
    const apiError = error as ApiError;
    expect(apiError.status).toBe(409);
    expect(apiError.code).toBe("invalid_state");
    expect(apiError.message).toBe("state is merged");
    expect(apiError.fields).toEqual({ state: "expected escalated or rejected" });
  });

  it("keeps extra 409 fields on the error body", async () => {
    server.use(
      http.post("/api/v1/settings/restart", () =>
        HttpResponse.json(
          {
            error: { code: "conflict", message: "active runs" },
            active_runs: [
              {
                id: "run-1",
                fingerprint: "fp-1",
                runner_id: "r1",
                repo: "a/b",
                phase: "repair",
                started_at_ms: 1,
              },
            ],
          },
          { status: 409 },
        ),
      ),
    );

    await expect(
      apiFetch("/settings/restart", { method: "POST", body: "{}" }),
    ).rejects.toMatchObject({
      status: 409,
      body: { active_runs: [{ id: "run-1" }] },
    });
  });

  it("exposes Retry-After seconds on rate-limit errors", async () => {
    server.use(
      http.post("/api/v1/auth/login", () =>
        HttpResponse.json(
          { error: { code: "rate_limited", message: "retry after 30000ms" } },
          { status: 429, headers: { "Retry-After": "30" } },
        ),
      ),
    );

    const error = (await apiFetch("/auth/login", { method: "POST" }).catch(
      (reason: unknown) => reason,
    )) as ApiError;

    expect(error.status).toBe(429);
    expect(error.retryAfterSeconds).toBe(30);
  });

  it("calls the unauthorized handler for a 401 outside auth endpoints", async () => {
    const handler = vi.fn();
    restoreHandler = setUnauthorizedHandler(handler);
    server.use(
      http.get("/api/v1/overview", () =>
        HttpResponse.json(
          { error: { code: "unauthorized", message: "session expired" } },
          { status: 401 },
        ),
      ),
    );

    await expect(apiFetch("/overview")).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
    });
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("leaves auth-endpoint 401s to the auth flow", async () => {
    const handler = vi.fn();
    restoreHandler = setUnauthorizedHandler(handler);
    server.use(
      http.get("/api/v1/auth/me", () =>
        HttpResponse.json(
          { error: { code: "unauthorized", message: "not logged in" } },
          { status: 401 },
        ),
      ),
    );

    await expect(apiFetch("/auth/me")).rejects.toMatchObject({ status: 401 });
    expect(handler).not.toHaveBeenCalled();
  });

  it("resolves undefined for 204 responses", async () => {
    server.use(http.post("/api/v1/auth/logout", () => new HttpResponse(null, { status: 204 })));
    await expect(apiFetch("/auth/logout", { method: "POST" })).resolves.toBeUndefined();
  });

  it("rejects absolute URLs so requests cannot leave the origin", async () => {
    await expect(apiFetch("https://example.com/api")).rejects.toThrow(/API-relative/);
  });
});
