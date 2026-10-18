/**
 * Transcript labels and formatting. The server already truncates every block
 * at 16KB; the viewer additionally collapses long-but-legal content so one
 * message cannot fill the screen.
 */

import { formatAge, formatDateTime } from "../overview/format";
import type { TranscriptRole } from "./types";

export { formatAge, formatDateTime };

const ROLE_LABELS: Record<TranscriptRole, string> = {
  user: "用户",
  assistant: "助手",
  tool: "工具",
};

export function transcriptRoleLabel(role: TranscriptRole): string {
  return ROLE_LABELS[role];
}

/** Show full text up to this many characters; longer text starts collapsed. */
export const LONG_TEXT_CHARS = 900;

/** Preview budget for a collapsed block, in characters. */
export const LONG_TEXT_PREVIEW_CHARS = 420;

export function isLongText(text: string): boolean {
  return text.length > LONG_TEXT_CHARS;
}

export function previewText(text: string, limit: number = LONG_TEXT_PREVIEW_CHARS): string {
  if (text.length <= limit) {
    return text;
  }
  return `${text.slice(0, limit)}…`;
}

/**
 * Tool-call arguments are either a JSON value or the server's short string
 * preview. Pretty-print objects without ever throwing on cyclic/invalid data.
 */
export function formatToolArguments(value: unknown): string {
  if (typeof value === "string") {
    return value;
  }
  if (value === undefined) {
    return "—";
  }
  try {
    const json = JSON.stringify(value, null, 2);
    return json ?? String(value);
  } catch {
    return String(value);
  }
}

/** Message timestamp: relative label with the absolute time in the tooltip. */
export function messageTimeTitle(tsMs: number): string {
  return formatDateTime(tsMs);
}
