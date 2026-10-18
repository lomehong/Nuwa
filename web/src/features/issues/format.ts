/**
 * Small formatting helpers for the issue list. The overview has its own copy
 * for its feed; keeping the list independent avoids a cross-feature import for
 * three one-line formatters.
 */

const NUMBER_FORMAT = new Intl.NumberFormat("zh-CN");

export function formatCount(value: number): string {
  return NUMBER_FORMAT.format(Number.isFinite(value) ? value : 0);
}

export function isoDateTime(tsMs: number): string | undefined {
  return Number.isFinite(tsMs) ? new Date(tsMs).toISOString() : undefined;
}

export function formatDateTime(tsMs: number): string {
  if (!Number.isFinite(tsMs)) {
    return "—";
  }
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(tsMs));
}

/** Relative age: 刚刚 / 5 分钟前 / 3 天前 (absolute value in the tooltip). */
export function formatAge(tsMs: number, now: number = Date.now()): string {
  const diff = Number.isFinite(tsMs) ? Math.max(0, now - tsMs) : 0;
  const seconds = Math.floor(diff / 1_000);
  if (seconds < 60) {
    return "刚刚";
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes} 分钟前`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours} 小时前`;
  }
  const days = Math.floor(hours / 24);
  if (days < 30) {
    return `${days} 天前`;
  }
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).format(new Date(tsMs));
}

/** Copy-safe short fingerprint; the full value is available via `title`. */
export function shortFingerprint(fingerprint: string, length = 10): string {
  if (fingerprint.length <= length) {
    return fingerprint;
  }
  return `${fingerprint.slice(0, length)}…`;
}

export function issueDetailPath(fingerprint: string): string {
  return `/issues/${encodeURIComponent(fingerprint)}`;
}
