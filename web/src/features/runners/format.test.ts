import { describe, expect, it } from "vitest";

import { makeRunner } from "../../test/msw";
import {
  runnerIsOffline,
  runnerQuotaPercent,
  runnerStatusLabel,
  runnerStatusVariant,
} from "./format";

const NOW = 1_700_000_000_000;

describe("runner status", () => {
  it("marks a stale heartbeat offline regardless of the stored status", () => {
    const runner = makeRunner({ status: "working", heartbeat_at_ms: NOW - 91_000 });
    expect(runnerStatusLabel(runner, NOW)).toBe("离线");
    expect(runnerStatusVariant(runner, NOW)).toBe("danger");
  });

  it("maps live statuses to operator vocabulary", () => {
    expect(runnerStatusLabel(makeRunner({ status: "starting" }), NOW)).toBe("启动中");
    expect(runnerStatusLabel(makeRunner({ status: "idle" }), NOW)).toBe("待命");
    expect(runnerStatusLabel(makeRunner({ status: "standing_down" }), NOW)).toBe("配额待命");
    expect(runnerStatusLabel(makeRunner({ status: "error" }), NOW)).toBe("异常");
    expect(runnerStatusLabel(makeRunner({ status: "working" }), NOW)).toBe("工作中");
  });

  it("treats an explicit offline flag or a future heartbeat as offline only when flagged", () => {
    expect(runnerIsOffline(makeRunner({ offline: true }), NOW)).toBe(true);
    expect(runnerIsOffline(makeRunner({ heartbeat_at_ms: NOW + 5_000 }), NOW)).toBe(false);
    expect(runnerIsOffline(makeRunner({ heartbeat_at_ms: NOW - 90_000 }), NOW)).toBe(false);
  });

  it("falls back to the raw status for unknown values", () => {
    expect(runnerStatusLabel(makeRunner({ status: "draining" }), NOW)).toBe("draining");
    expect(runnerStatusVariant(makeRunner({ status: "draining" }), NOW)).toBe("neutral");
  });

  it("computes the quota percentage without dividing by zero", () => {
    expect(runnerQuotaPercent(makeRunner({ prs_today: 2, daily_pr_limit: 5 }))).toBe(40);
    expect(runnerQuotaPercent(makeRunner({ prs_today: 2, daily_pr_limit: null }))).toBeNull();
    expect(runnerQuotaPercent(makeRunner({ prs_today: 2, daily_pr_limit: 0 }))).toBeNull();
    expect(runnerQuotaPercent(makeRunner({ prs_today: 9, daily_pr_limit: 5 }))).toBe(100);
  });
});
