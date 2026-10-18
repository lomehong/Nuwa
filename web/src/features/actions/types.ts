/**
 * Wire shape for `POST /api/v1/sync` (`crates/nuwa/src/ci.rs::SyncSummary`).
 *
 * The server serializes `errors` as `{ fingerprint, message }` objects
 * (docs/09 §9.2). A preformatted `"fp — message"` string is also accepted:
 * older fixtures and logs use that form, and a string must never reach the
 * toast as `[object Object]`.
 */

export interface SyncError {
  fingerprint: string;
  message: string;
}

export type SyncErrorWire = SyncError | string;

export interface SyncResponse {
  checked: number;
  merged: number;
  rejected: number;
  still_open: number;
  errors: SyncErrorWire[];
}

/** One line of per-issue error text, matching the reconciler's `fp — message`. */
export function syncErrorText(error: SyncErrorWire): string {
  if (typeof error === "string") {
    return error;
  }
  return error.fingerprint ? `${error.fingerprint} — ${error.message}` : error.message;
}
