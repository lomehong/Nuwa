import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { probeServeOnce, waitForServeRecovery } from "./health";

type FetchMock = ReturnType<typeof vi.fn>;

function response(ok: boolean): Response {
  return { ok } as Response;
}

function mockFetch(...answers: Array<Response | Error | (() => Response | Error)>): FetchMock {
  const fetchMock = vi.fn(async () => {
    const next = answers.shift();
    if (!next) {
      throw new Error("unexpected fetch");
    }
    const value = typeof next === "function" ? next() : next;
    if (value instanceof Error) {
      throw value;
    }
    return value;
  });
  vi.spyOn(globalThis, "fetch").mockImplementation(fetchMock as typeof fetch);
  return fetchMock;
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("probeServeOnce", () => {
  it("requires both /health and /auth/me", async () => {
    const fetchMock = mockFetch(response(true), response(true));

    await expect(probeServeOnce()).resolves.toBe(true);
    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "/health",
      expect.objectContaining({ cache: "no-store", credentials: "same-origin" }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "/api/v1/auth/me",
      expect.objectContaining({ cache: "no-store", credentials: "include" }),
    );
  });

  it("treats a failed health check or a dead session as not recovered", async () => {
    mockFetch(response(false));
    await expect(probeServeOnce()).resolves.toBe(false);

    mockFetch(new Error("connection reset"));
    await expect(probeServeOnce()).resolves.toBe(false);

    mockFetch(response(true), response(false));
    await expect(probeServeOnce()).resolves.toBe(false);
  });
});

describe("waitForServeRecovery", () => {
  it("returns immediately when serve and the session are already back", async () => {
    const fetchMock = mockFetch(response(true), response(true));

    await expect(waitForServeRecovery(5_000)).resolves.toBeUndefined();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("retries until health and /auth/me both answer", async () => {
    // First poll: process still down. Second: health is up but the session
    // endpoint has not come back yet. Third: fully recovered.
    mockFetch(
      response(false),
      response(true),
      response(false),
      response(true),
      response(true),
    );

    const recovery = waitForServeRecovery(10_000);
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(1_000);
    await vi.advanceTimersByTimeAsync(1_000);

    await expect(recovery).resolves.toBeUndefined();
  });

  it("throws once the timeout elapses", async () => {
    mockFetch(response(false), response(false), response(false), response(false));

    const recovery = waitForServeRecovery(2_000);
    // A rejection must be observed before advancing or vitest reports it as
    // unhandled when the loop throws.
    const settled = recovery.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(2_000);

    const error = await settled;
    expect(error).toBeInstanceOf(Error);
    expect((error as Error).message).toBe("服务在 2 秒内未恢复");
  });
});
