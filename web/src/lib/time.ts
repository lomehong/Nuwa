/**
 * Timestamp guards. `Date` accepts ±100,000,000 days around the epoch; a
 * finite value outside that range makes `toISOString()` / `Intl.format()`
 * throw a RangeError, which would blank the page through the error boundary.
 */

const MAX_TIMESTAMP_MS = 8_640_000_000_000_000;

export function isValidTimestamp(tsMs: number): boolean {
  return Number.isFinite(tsMs) && Math.abs(tsMs) <= MAX_TIMESTAMP_MS;
}

/** ISO-8601 string for a valid instant; `undefined` when `Date` cannot hold it. */
export function isoDateTime(tsMs: number): string | undefined {
  return isValidTimestamp(tsMs) ? new Date(tsMs).toISOString() : undefined;
}
