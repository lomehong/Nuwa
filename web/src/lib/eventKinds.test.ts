import { describe, expect, it } from "vitest";

import type { BadgeVariant } from "../components/ui";
import { decisionDetailLabel, eventKindLabel, eventKindVariant } from "./eventKinds";

/**
 * Every kind the backend writes plus the legacy kinds still present in older
 * databases. The tone table is canonical: the feed, recent events and the
 * issue timeline all consume it.
 */
const CASES: readonly [kind: string, label: string, variant: BadgeVariant][] = [
  ["reported", "上报", "neutral"],
  ["state_changed", "状态变更", "info"],
  ["requeued", "重新排队", "warning"],
  ["reopened", "重开", "warning"],
  ["pr_opened", "PR 已开", "info"],
  ["merged", "已合并", "success"],
  ["rejected", "已拒绝", "danger"],
  ["ci_green", "CI 通过", "success"],
  ["ci_red", "CI 失败", "danger"],
  ["ci_timeout", "CI 超时", "warning"],
  ["config_changed", "配置变更", "neutral"],
  ["config_migrated", "配置迁移", "neutral"],
  ["restart_requested", "请求重启", "warning"],
  ["runner_started", "运行器启动", "info"],
  ["runner_stopped", "运行器停止", "neutral"],
  ["runner_error", "运行器异常", "danger"],
  ["sync", "对账", "neutral"],
  ["decision", "决策", "info"],
  ["pr_merged", "PR 已合并", "success"],
  ["pr_closed", "PR 已关闭", "danger"],
];

describe("event kind catalog", () => {
  it.each(CASES)("maps %s to %s / %s", (kind, label, variant) => {
    expect(eventKindLabel(kind)).toBe(label);
    expect(eventKindVariant(kind)).toBe(variant);
  });

  it("leaves unknown kinds neutral without losing the raw label", () => {
    expect(eventKindLabel("unknown_kind")).toBe("unknown_kind");
    expect(eventKindVariant("unknown_kind")).toBe("neutral");
  });

  it("renders a decision trace as one readable line", () => {
    expect(
      decisionDetailLabel(
        JSON.stringify({ stage: "deliver", decision: "pr_opened", reason: "verification gate green" }),
      ),
    ).toBe("deliver · pr_opened — verification gate green");
    expect(decisionDetailLabel("not json")).toBe("not json");
    expect(decisionDetailLabel(JSON.stringify({ reason: "missing stage" }))).toBe(
      JSON.stringify({ reason: "missing stage" }),
    );
  });
});
