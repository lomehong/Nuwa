/**
 * Project formatting helpers. The project list is dense, so names are
 * shortened consistently: the service name is the last path segment, the full
 * identity stays available in the mono caption.
 */

import { isValidTimestamp } from "../../lib/time";

/** `example.com/acme/ingest` → `ingest`. */
export function projectShortName(repo: string): string {
  const segments = repo.split("/").filter(Boolean);
  return segments.length > 0 ? segments[segments.length - 1] : repo;
}

/**
 * The project's display name: the registry name, else the service name. A
 * registry that only repeats the repo id adds nothing, so it falls back to
 * the service name and the full identity stays in the mono caption.
 */
export function projectLabel(name: string | null, repo: string): string {
  const trimmed = name?.trim();
  if (!trimmed || trimmed === repo) {
    return projectShortName(repo);
  }
  return trimmed;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** "3 分钟前" / "2 天前" — the project's last signal. */
export function formatLastSignal(tsMs: number, now: number = Date.now()): string {
  if (!isValidTimestamp(tsMs)) {
    return "—";
  }
  const diff = Math.max(0, now - tsMs);
  if (diff < MINUTE) {
    return "刚刚";
  }
  if (diff < HOUR) {
    return `${Math.floor(diff / MINUTE)} 分钟前`;
  }
  if (diff < DAY) {
    return `${Math.floor(diff / HOUR)} 小时前`;
  }
  return `${Math.floor(diff / DAY)} 天前`;
}
