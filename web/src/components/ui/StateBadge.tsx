import { clsx } from "clsx";
import {
  ArrowUpFromLine,
  EyeOff,
  GitMerge,
  GitPullRequest,
  Hammer,
  Plus,
  X,
  Zap,
  type LucideIcon,
} from "lucide-react";

export const ISSUE_STATE_VALUES = [
  "new",
  "triggered",
  "fixing",
  "pr_open",
  "merged",
  "rejected",
  "escalated",
  "ignored",
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
  ignored: "已忽略",
};

/** Shape channel (docs/09 §11.2 "never colour alone"): a green merged badge
 * and a green CI-pass badge share a hue, never a silhouette. */
const ISSUE_STATE_ICONS: Record<IssueState, LucideIcon> = {
  new: Plus,
  triggered: Zap,
  fixing: Hammer,
  pr_open: GitPullRequest,
  merged: GitMerge,
  rejected: X,
  escalated: ArrowUpFromLine,
  ignored: EyeOff,
};

export function issueStateLabel(state: IssueState): string {
  return ISSUE_STATE_LABELS[state];
}

export interface StateBadgeProps {
  state: IssueState;
  className?: string;
}

/**
 * Issue state badge: per-state icon + label + tinted colour. The `state-*`
 * class maps the dynamic value to CSS variables, so no inline style is needed
 * (the strict CSP forbids one).
 */
export function StateBadge({ state, className }: StateBadgeProps) {
  const Icon = ISSUE_STATE_ICONS[state];
  return (
    <span className={clsx("state-badge", `state-${state}`, className)} data-state={state}>
      <Icon aria-hidden="true" className="state-badge__icon size-3" />
      {issueStateLabel(state)}
    </span>
  );
}
