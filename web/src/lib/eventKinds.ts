/**
 * Single catalog for event kind labels and badge tones (docs/09 §10.7).
 * The activity feed, the overview's recent-events panel and the issue timeline
 * all render the same backend kinds, so they must agree on both vocabulary and
 * tone. Unknown kinds fall back to neutral so a new backend kind never breaks
 * the UI.
 */

import type { BadgeVariant } from "../components/ui";

const EVENT_KIND_LABELS: Record<string, string> = {
  reported: "上报",
  state_changed: "状态变更",
  requeued: "重新排队",
  reopened: "重开",
  pr_opened: "PR 已开",
  merged: "已合并",
  rejected: "已拒绝",
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

const EVENT_KIND_VARIANTS: Record<string, BadgeVariant> = {
  reported: "neutral",
  state_changed: "info",
  requeued: "warning",
  reopened: "warning",
  pr_opened: "info",
  merged: "success",
  rejected: "danger",
  pr_merged: "success",
  pr_closed: "danger",
  ci_green: "success",
  ci_red: "danger",
  ci_timeout: "warning",
  config_changed: "neutral",
  config_migrated: "neutral",
  restart_requested: "warning",
  runner_started: "info",
  runner_stopped: "neutral",
  runner_error: "danger",
  sync: "neutral",
};

export function eventKindVariant(kind: string): BadgeVariant {
  return EVENT_KIND_VARIANTS[kind] ?? "neutral";
}
