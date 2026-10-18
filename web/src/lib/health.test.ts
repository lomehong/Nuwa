import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  RECOVERY_INITIAL_DELAY_MS,
  SessionExpiredDuringRecoveryError,
  navigateToLogin,
  probeServeOnce,
  reloadConsole,
  waitForServeRecovery,
} from "./health";

type FetchMock = ReturnType<typeof vi.fn>;

function response(ok: boolean, status = ok ? 200 : 503): Response {
  return { ok, status } as Response;
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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
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

  it("fails immediately when health is back but /auth/me rejects the session", async () => {
    mockFetch(response(true), response(false, 401));

    await expect(probeServeOnce()).rejects.toBeInstanceOf(
      SessionExpiredDuringRecoveryError,
    );
  });

  it("bounds each probe with a timeout signal that also follows the caller", async () => {
    const external = new AbortController();
    const signals: AbortSignal[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation((async (
      _input: RequestInfo | URL,
      init?: RequestInit,
    ) => {
      if (init?.signal) {
        signals.push(init.signal);
      }
      return response(true);
    }) as typeof fetch);

    await expect(probeServeOnce(external.signal)).resolves.toBe(true);
    expect(signals).toHaveLength(2);
    for (const signal of signals) {
      // Combined with AbortSignal.timeout, so never the caller's signal itself.
      expect(signal).not.toBe(external.signal);
      expect(signal.aborted).toBe(false);
    }

    external.abort();
    expect(signals.every((signal) => signal.aborted)).toBe(true);
  });
});

describe("waitForServeRecovery", () => {
  it("waits out the systemd restart delay before the first probe", async () => {
    const fetchMock = mockFetch(response(true), response(true));

    const recovery = waitForServeRecovery();
    await vi.advanceTimersByTimeAsync(RECOVERY_INITIAL_DELAY_MS - 1);
    // The old process still serves /health during systemd-run's 2s delay, so
    // probing earlier would reload against a process about to die.
    expect(fetchMock).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    await expect(recovery).resolves.toBeUndefined();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("retries every second until health and /auth/me both answer", async () => {
    // First poll at 3s: health down. Second at 4s: health up, session not yet.
    // Third at 5s: fully recovered.
    mockFetch(
      response(false),
      response(true),
      response(false),
      response(true),
      response(true),
    );

    const recovery = waitForServeRecovery(undefined, 10_000);
    await vi.advanceTimersByTimeAsync(RECOVERY_INITIAL_DELAY_MS);
    await vi.advanceTimersByTimeAsync(1_000);
    await vi.advanceTimersByTimeAsync(1_000);

    await expect(recovery).resolves.toBeUndefined();
  });

  it("throws once the timeout elapses", async () => {
    mockFetch(response(false), response(false), response(false), response(false));

    const recovery = waitForServeRecovery(undefined, 5_000);
    // A rejection must be observed before advancing or vitest reports it as
    // unhandled when the loop throws.
    const settled = recovery.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(5_000);

    const error = await settled;
    expect(error).toBeInstanceOf(Error);
    expect((error as Error).message).toBe("服务在 5 秒内未恢复");
  });

  it("stops immediately when the restored process rejects the old session", async () => {
    const fetchMock = mockFetch(response(true), response(false, 401));

    const recovery = waitForServeRecovery(undefined, 10_000);
    const settled = recovery.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(RECOVERY_INITIAL_DELAY_MS);

    const error = await settled;
    expect(error).toBeInstanceOf(SessionExpiredDuringRecoveryError);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    // A rotated token never accepts the old session, so no further poll runs.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("stops polling and rejects when the caller aborts", async () => {
    const controller = new AbortController();
    const fetchMock = mockFetch(response(true), response(true));

    const recovery = waitForServeRecovery(controller.signal);
    const settled = recovery.catch((error: unknown) => error);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(fetchMock).not.toHaveBeenCalled();

    controller.abort();
    const error = await settled;
    expect(error).toMatchObject({ name: "AbortError" });

    await vi.advanceTimersByTimeAsync(120_000);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("reloadConsole", () => {
  it("navigates to the SPA root instead of reloading the current path", () => {
    const assign = vi.fn();
    // jsdom's Location.assign is non-configurable, so replace the whole window
    // binding instead of trying to spy on the method.
    vi.stubGlobal("window", { ...window, location: { assign } });

    reloadConsole();

    expect(assign).toHaveBeenCalledWith("/");
  });
});

describe("navigateToLogin", () => {
  it("navigates to /login after a token rotation", () => {
    const assign = vi.fn();
    vi.stubGlobal("window", { ...window, location: { assign } });

    navigateToLogin();

    expect(assign).toHaveBeenCalledWith("/login");
  });
});
