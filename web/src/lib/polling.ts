/**
 * Polling cadence shared by the console's data hooks.
 *
 * A healthy query polls at its page-defined base interval. After consecutive
 * fetch failures the interval doubles (base, 2x, 4x, …) and caps at 60s so a
 * failing `serve` is not hammered, while a recovered query returns to base as
 * soon as `fetchFailureCount` resets to zero.
 */

export const DEFAULT_MAX_BACKOFF_MS = 60_000;

/** Delay for `failures` consecutive failures: 0 -> base, 1 -> 2x, capped. */
export function backoffDelay(
  baseMs: number,
  failures: number,
  maxMs = DEFAULT_MAX_BACKOFF_MS,
): number {
  if (failures <= 0) {
    return baseMs;
  }
  return Math.min(baseMs * 2 ** failures, maxMs);
}

/** TanStack Query `refetchInterval` callback driven by the failure count. */
export function backoffInterval(baseMs: number) {
  return (query: { state: { fetchFailureCount: number } }): number =>
    backoffDelay(baseMs, query.state.fetchFailureCount);
}
