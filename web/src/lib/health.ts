/**
 * Serve-recovery probe used after a guarded restart (docs/09 §10.8, §13.3).
 *
 * `POST /settings/restart` answers 202 before systemd restarts the process, so
 * the console cannot trust the still-open page or its session. It polls the
 * public `/health` endpoint every second, and only accepts recovery once
 * `/auth/me` also succeeds: a fresh process may answer `/health` before the
 * session cookie is verified, and reloading against that state would bounce
 * the operator to `/login`.
 *
 * Plain `fetch` is deliberate. `apiFetch` would surface a transient 401 as a
 * session expiry and redirect to the login page mid-restart.
 */

export const RECOVERY_POLL_MS = 1_000;
export const RECOVERY_TIMEOUT_MS = 60_000;

async function isOk(path: string, credentials: RequestCredentials): Promise<boolean> {
  try {
    const response = await fetch(path, { cache: "no-store", credentials });
    return response.ok;
  } catch {
    // Connection reset while the process restarts is the expected case.
    return false;
  }
}

/**
 * One recovery probe: `/health` proves the serve process is back, `/auth/me`
 * proves the session survived the restart. Exported so tests can exercise the
 * two steps without waiting on timers.
 */
export async function probeServeOnce(): Promise<boolean> {
  if (!(await isOk("/health", "same-origin"))) {
    return false;
  }
  return isOk("/api/v1/auth/me", "include");
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
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
 * reject once `timeoutMs` has elapsed. Polls once immediately, then every
 * second.
 */
export async function waitForServeRecovery(timeoutMs = RECOVERY_TIMEOUT_MS): Promise<void> {
  const deadline = Date.now() + Math.max(0, timeoutMs);
  while (Date.now() < deadline) {
    if (await probeServeOnce()) {
      return;
    }
    const remaining = deadline - Date.now();
    await sleep(Math.min(RECOVERY_POLL_MS, Math.max(0, remaining)));
  }
  const seconds = Math.max(0, Math.round(timeoutMs / 1_000));
  throw new Error(`服务在 ${seconds} 秒内未恢复`);
}
