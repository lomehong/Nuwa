/**
 * Transcript types for `GET /api/v1/issues/{fp}/transcript` (docs/09 §9.2).
 *
 * `arguments` intentionally stays `unknown`: the server normally sends a JSON
 * object but replaces a pathological argument blob with a short
 * `<truncated: N bytes>` string. Both must render without throwing.
 */

export const TRANSCRIPT_ROLES = ["user", "assistant", "tool"] as const;

export type TranscriptRole = (typeof TRANSCRIPT_ROLES)[number];

export interface ThinkingBlock {
  type: "thinking";
  text: string;
  truncated?: boolean;
}

export interface TextBlock {
  type: "text";
  text: string;
  truncated?: boolean;
}

export interface ToolCallBlock {
  type: "tool_call";
  name: string;
  arguments: unknown;
  truncated?: boolean;
}

export interface ToolResultBlock {
  type: "tool_result";
  name: string;
  text: string;
  is_error: boolean;
  truncated?: boolean;
}

export type TranscriptBlock = ThinkingBlock | TextBlock | ToolCallBlock | ToolResultBlock;

export interface TranscriptMessage {
  seq: number;
  role: TranscriptRole;
  ts_ms: number;
  blocks: TranscriptBlock[];
}

export interface TranscriptMeta {
  page: number;
  page_size: number;
  total: number;
  /** The session file exceeded the server's 2MB tail window. */
  truncated: boolean;
  /** No run with a session, no JSONL file, or no displayable messages. */
  empty: boolean;
}

export interface TranscriptResponse {
  data: TranscriptMessage[];
  meta: TranscriptMeta;
}
