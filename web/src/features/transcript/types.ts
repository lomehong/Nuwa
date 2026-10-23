/**
 * Transcript types for `GET /api/v1/issues/{fp}/transcript` (docs/09 §9.2).
 *
 * Blocks come from the Rust DTO via ts-rs: a discriminated union on `type`
 * whose `arguments` is a JSON value (the server may replace a pathological
 * argument blob with a short `<truncated: N bytes>` string — both must render
 * without throwing).
 */

import type { TranscriptBlock as ApiTranscriptBlock } from "../../lib/api-types/TranscriptBlock";
import type { TranscriptMessage as ApiTranscriptMessage } from "../../lib/api-types/TranscriptMessage";
import type { TranscriptResponse as ApiTranscriptResponse } from "../../lib/api-types/TranscriptResponse";

export const TRANSCRIPT_ROLES = ["user", "assistant", "tool"] as const;

export type TranscriptRole = (typeof TRANSCRIPT_ROLES)[number];

/**
 * `truncated` is omitted from the wire when false, so the UI type makes it
 * optional (ts-rs ignores `skip_serializing_if` and keeps it required).
 */
type WithOptionalTruncated<T> = T extends { truncated: boolean }
  ? Omit<T, "truncated"> & { truncated?: boolean }
  : T;

export type TranscriptBlock = WithOptionalTruncated<ApiTranscriptBlock>;
export type { TranscriptMeta } from "../../lib/api-types/TranscriptMeta";

export type TranscriptMessage = Omit<ApiTranscriptMessage, "role" | "blocks"> & {
  role: TranscriptRole;
  blocks: TranscriptBlock[];
};

export type TranscriptResponse = Omit<ApiTranscriptResponse, "data"> & {
  data: TranscriptMessage[];
};
