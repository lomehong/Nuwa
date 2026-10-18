import { clsx } from "clsx";

export const ISSUE_STATE_VALUES = [
  "new",
  "triggered",
  "fixing",
  "pr_open",
  "merged",
  "rejected",
  "escalated",
] as const;

export type IssueState = (typeof ISSUE_STATE_VALUES)[number];

const ISSUE_STATE_LABELS: Record<IssueState, string> = {
  new: "新建",
  triggered: "已触发",
  fixing: "修复中",
  pr_open: "PR 待合并",
  merged: "已合并",
  rejected: "已拒绝",
  escalated: "已升级",
};

export function issueStateLabel(state: IssueState): string {
  return ISSUE_STATE_LABELS[state];
}

export interface StateBadgeProps {
  state: IssueState;
  className?: string;
}

/**
 * Issue state badge: dot + label + tinted colour. The `state-*` class maps the
 * dynamic value to CSS variables, so no inline style is needed.
 */
export function StateBadge({ state, className }: StateBadgeProps) {
  return (
    <span className={clsx("state-badge", `state-${state}`, className)} data-state={state}>
      <span aria-hidden="true" className="state-badge__dot" />
      {issueStateLabel(state)}
    </span>
  );
}
