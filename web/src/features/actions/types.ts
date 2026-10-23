/**
 * Wire shape for `POST /api/v1/sync` (`crates/nuwa/src/ci.rs::SyncSummary`).
 *
 * `errors` is generated from the Rust DTO (`{ fingerprint, message }`). A
 * preformatted `"fp — message"` string is also accepted: older fixtures and
 * logs use that form, and a string must never reach the toast as
 * `[object Object]`.
 */

import type { SyncError } from "../../lib/api-types/SyncError";
import type { SyncSummary } from "../../lib/api-types/SyncSummary";

export type { SyncError };

export type SyncErrorWire = SyncError | string;

export type SyncResponse = Omit<SyncSummary, "errors"> & {
  errors: SyncErrorWire[];
};

/** One line of per-issue error text, matching the reconciler's `fp — message`. */
export function syncErrorText(error: SyncErrorWire): string {
  if (typeof error === "string") {
    return error;
  }
  return error.fingerprint ? `${error.fingerprint} — ${error.message}` : error.message;
}
