/**
 * Small, locale-stable formatting helpers for the overview. Keeping them in
 * one module makes the rendered strings unit-testable and avoids a formatting
 * library (the page only needs counts, durations and relative timestamps).
 */

const NUMBER_FORMAT = new Intl.NumberFormat("zh-CN");

export function formatNumber(value: number): string {
  return NUMBER_FORMAT.format(Number.isFinite(value) ? value : 0);
}

/** Compact token counts: 950, 17.5k, 1.2M. */
export function formatTokens(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return "0";
  }
  if (value >= 1_000_000) {
    return `${(value / 1_000_000).toFixed(1)}M`;
  }
  if (value >= 1_000) {
    return `${(value / 1_000).toFixed(1)}k`;
  }
  return String(Math.round(value));
}

/** Elapsed run time: 45 秒 / 3 分 20 秒 / 2 小时 5 分 / 1 天 3 小时. */
export function formatDuration(ms: number): string {
  const seconds = Math.max(0, Math.floor((Number.isFinite(ms) ? ms : 0) / 1_000));
  if (seconds < 60) {
    return `${seconds} 秒`;
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes} 分 ${seconds % 60} 秒`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours} 小时 ${minutes % 60} 分`;
  }
  const days = Math.floor(hours / 24);
  return `${days} 天 ${hours % 24} 小时`;
}

/** Relative age for attention cards and events: 刚刚 / 5 分钟前 / 3 天前. */
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
  return formatDate(tsMs);
}

export function formatDate(tsMs: number): string {
  if (!Number.isFinite(tsMs)) {
    return "—";
  }
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).format(new Date(tsMs));
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

/** `2026-10-17` → `10-17` for compact chart labels and tooltips. */
export function formatDayShort(day: string): string {
  const match = /^\d{4}-(\d{2}-\d{2})$/.exec(day);
  return match ? match[1] : day;
}

/** Short, copy-safe fingerprint shown under titles; the full value is a title. */
export function shortFingerprint(fingerprint: string, length = 10): string {
  if (fingerprint.length <= length) {
    return fingerprint;
  }
  return `${fingerprint.slice(0, length)}…`;
}

/**
 * System-owned event fingerprints (`__config__`, `__system__`,
 * `__runner__:<id>`) have no issue detail page to link to (docs/09 §10.7).
 */
export function isSystemFingerprint(fingerprint: string): boolean {
  return (
    fingerprint === "__config__" ||
    fingerprint === "__system__" ||
    fingerprint.startsWith("__runner__:")
  );
}

const EVENT_KIND_LABELS: Record<string, string> = {
  reported: "上报",
  state_changed: "状态变更",
  requeued: "重新排队",
  reopened: "重开",
  pr_opened: "PR 已开",
  pr_merged: "PR 已合并",
  pr_closed: "PR 已关闭",
  ci_green: "CI 通过",
  ci_red: "CI 失败",
  ci_timeout: "CI 超时",
  config_changed: "配置变更",
  config_migrated: "配置迁移",
  restart_requested: "请求重启",
  runner_started: "运行器启动",
  runner_stopped: "运行器停止",
  runner_error: "运行器异常",
  sync: "对账",
};

export function eventKindLabel(kind: string): string {
  return EVENT_KIND_LABELS[kind] ?? kind;
}
