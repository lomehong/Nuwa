/**
 * Labels and small formatters for the issue detail page. Time, duration and
 * token helpers are shared with the overview instead of being re-derived;
 * only detail-specific vocabulary (run phase/outcome, event tone) lives here.
 */

import { ApiError } from "../../lib/api";
import type { BadgeVariant } from "../../components/ui";
import { formatCount, shortFingerprint } from "../issues/format";
import {
  eventKindLabel,
  formatAge,
  formatDateTime,
  formatDuration,
  formatTokens,
  isSystemFingerprint,
} from "../overview/format";
import type { RunDto } from "./types";

export {
  eventKindLabel,
  formatAge,
  formatCount,
  formatDateTime,
  formatDuration,
  formatTokens,
  isSystemFingerprint,
  shortFingerprint,
};

const RUN_PHASE_LABELS: Record<string, string> = {
  repair: "修复",
  verify: "验证",
  deliver: "交付",
  ci: "等待 CI",
  done: "已完成",
};

export function runPhaseLabel(phase: string): string {
  return RUN_PHASE_LABELS[phase] ?? (phase || "—");
}

const RUN_OUTCOME_LABELS: Record<string, string> = {
  success: "成功",
  repair_failed: "修复失败",
  delivery_failed: "交付失败",
  budget_exceeded: "预算超限",
  ci_red: "CI 未通过",
  interrupted: "已中断",
};

export function runOutcomeLabel(outcome: string | null): string {
  if (!outcome) {
    return "进行中";
  }
  return RUN_OUTCOME_LABELS[outcome] ?? outcome;
}

export function runOutcomeVariant(outcome: string | null): BadgeVariant {
  switch (outcome) {
    case "success":
      return "success";
    case "ci_red":
    case "repair_failed":
    case "delivery_failed":
      return "danger";
    case "budget_exceeded":
    case "interrupted":
      return "warning";
    case null:
      return "info";
    default:
      return "neutral";
  }
}

export function runPhaseVariant(phase: string): BadgeVariant {
  return phase === "done" ? "neutral" : "info";
}

/** Finished or still-running elapsed time. */
export function runDurationMs(run: RunDto, now: number = Date.now()): number {
  const end = run.finished_at_ms ?? now;
  return Math.max(0, end - run.started_at_ms);
}

export function runTokenTotal(run: RunDto): number {
  return Math.max(0, run.tokens_uncached) + Math.max(0, run.tokens_cached);
}

/** Short run id for dense rows; the full id stays available via title. */
export function shortRunId(id: string, length = 18): string {
  if (id.length <= length) {
    return id;
  }
  return `${id.slice(0, length)}…`;
}

const TIMELINE_TONES: Record<string, BadgeVariant> = {
  reported: "neutral",
  state_changed: "info",
  requeued: "warning",
  reopened: "warning",
  pr_opened: "info",
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

export function timelineKindVariant(kind: string): BadgeVariant {
  return TIMELINE_TONES[kind] ?? "neutral";
}

const ACTOR_LABELS: Record<string, string> = {
  owner: "所有者",
  runner: "运行器",
  system: "系统",
  sync: "对账",
};

export function actorLabel(actor: string | null): string | null {
  if (!actor) {
    return null;
  }
  if (isSystemFingerprint(actor)) {
    return "系统";
  }
  return ACTOR_LABELS[actor] ?? actor;
}

export function isNotFoundError(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404;
}

export function describeDetailError(error: unknown): {
  message: string;
  code: string | null;
} {
  if (error instanceof ApiError) {
    return { message: error.message || "请求失败", code: error.code };
  }
  if (error instanceof Error && error.message) {
    return { message: error.message, code: null };
  }
  return { message: "请求失败，请稍后重试", code: null };
}
