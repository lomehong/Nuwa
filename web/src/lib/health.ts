/**
 * Serve-recovery probe used after a guarded restart (docs/09 §10.8, §13.3).
 *
 * `POST /settings/restart` answers 202 before systemd restarts the process, so
 * the console cannot trust the still-open page or its session. It waits out
 * the scheduled restart first (`systemd-run --on-active=2` leaves the old
 * process answering `/health` for a couple of seconds), then polls the public
 * `/health` endpoint every second, and only accepts recovery once `/auth/me`
 * also succeeds: a fresh process may answer `/health` before the session
 * cookie is verified, and reloading against that state would bounce the
 * operator to `/login`.
 *
 * Plain `fetch` is deliberate. `apiFetch` would surface a transient 401 as a
 * session expiry and redirect to the login page mid-restart.
 */

export const RECOVERY_POLL_MS = 1_000;
/** systemd-run fires the restart after 2s; probing earlier can only hit the
 * old process, which still answers `/health` and would reload too soon. */
export const RECOVERY_INITIAL_DELAY_MS = 3_000;
/** Every probe (health + session) is bounded so a hanging connection cannot
 * stall the loop past its deadline. */
export const RECOVERY_PROBE_TIMEOUT_MS = 2_000;
export const RECOVERY_TIMEOUT_MS = 60_000;

function abortError(): DOMException {
  return new DOMException("已取消等待服务恢复", "AbortError");
}

async function isOk(
  path: string,
  credentials: RequestCredentials,
  signal?: AbortSignal,
): Promise<boolean> {
  // `AbortSignal.any` keeps the caller's cancellation and the per-probe
  // timeout independent: either one aborts the fetch.
  const timeout = AbortSignal.timeout(RECOVERY_PROBE_TIMEOUT_MS);
  const combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
  try {
    const response = await fetch(path, { cache: "no-store", credentials, signal: combined });
    return response.ok;
  } catch {
    // Connection reset while the process restarts is the expected case; an
    // external abort is re-thrown by `waitForServeRecovery`, which owns the
    // decision to stop polling.
    return false;
  }
}

/**
 * One recovery probe: `/health` proves the serve process is back, `/auth/me`
 * proves the session survived the restart. Exported so tests can exercise the
 * two steps without waiting on timers.
 */
export async function probeServeOnce(signal?: AbortSignal): Promise<boolean> {
  if (!(await isOk("/health", "same-origin", signal))) {
    return false;
  }
  return isOk("/api/v1/auth/me", "include", signal);
}

/** Abortable sleep: rejecting on abort stops the loop before the next probe. */
function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) {
    return Promise.reject(abortError());
  }
  return new Promise((resolve, reject) => {
    function onAbort() {
      window.clearTimeout(timer);
      reject(abortError());
    }
    const timer = window.setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw abortError();
  }
}

/**
 * Hard reload after recovery. Isolated so the restart flow's decision is
 * observable in tests without jsdom's unimplemented navigation.
 */
export function reloadConsole(): void {
  window.location.reload();
}

/**
 * Resolve when the serve process and the console session are both back, or
 * reject once `timeoutMs` has elapsed. Waits `RECOVERY_INITIAL_DELAY_MS`
 * before the first probe (the old process is still serving), then polls every
 * second. Passing `signal` stops the loop on dialog unmount; the rejection is
 * a `DOMException` named `AbortError`, which callers treat as "not an error".
 */
export async function waitForServeRecovery(
  signal?: AbortSignal,
  timeoutMs = RECOVERY_TIMEOUT_MS,
): Promise<void> {
  const deadline = Date.now() + Math.max(0, timeoutMs);
  let delay = RECOVERY_INITIAL_DELAY_MS;
  for (;;) {
    throwIfAborted(signal);
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      break;
    }
    await sleep(Math.min(delay, remaining), signal);
    throwIfAborted(signal);
    if (Date.now() >= deadline) {
      break;
    }
    if (await probeServeOnce(signal)) {
      return;
    }
    throwIfAborted(signal);
    delay = RECOVERY_POLL_MS;
  }
  const seconds = Math.max(0, Math.round(timeoutMs / 1_000));
  throw new Error(`服务在 ${seconds} 秒内未恢复`);
}
