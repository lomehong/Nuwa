/**
 * Runner status semantics for the console.
 *
 * The API flags `offline` from its own clock, but the page polls every 10s and
 * may render minutes-old data: recompute staleness against the browser's `now`
 * so a runner whose heartbeat stopped still reads 离线 between polls. Future
 * heartbeats (negative age, e.g. clock skew) are treated as live.
 */

import type { BadgeVariant } from "../../components/ui";
import type { RunnerDto } from "./types";

/**
 * Keep in sync with `RUNNER_OFFLINE_MS` in `crates/nuwa/src/api/common.rs`;
 * the server and the console must agree on when a heartbeat is stale.
 */
export const RUNNER_OFFLINE_MS = 90_000;

const LABELS: Record<string, string> = {
  starting: "启动中",
  working: "工作中",
  idle: "待命",
  standing_down: "配额待命",
  error: "异常",
};

export function runnerIsOffline(runner: RunnerDto, nowMs: number): boolean {
  return nowMs - runner.heartbeat_at_ms > RUNNER_OFFLINE_MS || runner.offline;
}

export function runnerStatusLabel(runner: RunnerDto, nowMs: number): string {
  if (runnerIsOffline(runner, nowMs)) {
    return "离线";
  }
  return LABELS[runner.status] ?? runner.status;
}

export function runnerStatusVariant(runner: RunnerDto, nowMs: number): BadgeVariant {
  if (runnerIsOffline(runner, nowMs) || runner.status === "error") {
    return "danger";
  }
  switch (runner.status) {
    case "working":
      return "info";
    case "standing_down":
      return "warning";
    case "starting":
    case "idle":
      return "success";
    default:
      return "neutral";
  }
}

/** Daily PR usage as a whole percentage, or null when no limit is configured. */
export function runnerQuotaPercent(runner: RunnerDto): number | null {
  if (!runner.daily_pr_limit || runner.daily_pr_limit <= 0) {
    return null;
  }
  return Math.min(100, Math.round((runner.prs_today / runner.daily_pr_limit) * 100));
}
