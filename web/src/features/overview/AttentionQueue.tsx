import { ArrowRight, Clock3, ExternalLink, ShieldCheck } from "lucide-react";
import { Link } from "react-router-dom";

import {
  Badge,
  EmptyState,
  SeverityStone,
  StateBadge,
  type BadgeVariant,
} from "../../components/ui";
import { IssueActionButton } from "../issues/IssueActionButton";
import { formatAge, formatDateTime, isoDateTime } from "./format";
import { OverviewSection } from "./PanelChrome";
import type { OverviewAttentionItem } from "./types";

const REASON_VARIANTS: Record<string, BadgeVariant> = {
  repair_failed: "danger",
  delivery_failed: "danger",
  budget_exceeded: "warning",
  ci_red: "danger",
  pr_closed: "danger",
  stuck: "warning",
  ci_timeout: "warning",
};

export function issueDetailPath(fingerprint: string): string {
  return `/issues/${encodeURIComponent(fingerprint)}`;
}

interface AttentionQueueProps {
  items: OverviewAttentionItem[];
}

/**
 * "需要你处理" (docs/09 §10.3.1). Cards carry the server-provided reason text,
 * repository and age; the footer offers the state-aware owner action and the
 * link into the issue detail.
 */
export function AttentionQueue({ items }: AttentionQueueProps) {
  return (
    <OverviewSection
      id="overview-attention"
      title="需要你处理"
      description="升级、卡单、被拒绝或 CI 超时的问题单。"
      aside={
        <Badge variant={items.length > 0 ? "danger" : "neutral"}>
          待处理 <span className="font-mono tabular-nums">{items.length}</span>
        </Badge>
      }
    >
      {items.length === 0 ? (
        <EmptyState
          icon={ShieldCheck}
          title="暂无需要你处理的事项"
          description="问题单升级、运行器卡住、PR 被关闭或 CI 超时后会出现在这里。"
        />
      ) : (
        <ul className="ledger rounded-card border border-border bg-surface">
          {items.map((item) => {
            const detailPath = issueDetailPath(item.fingerprint);
            return (
              <li
                key={item.fingerprint}
                className="ledger__row flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2.5"
              >
                <SeverityStone
                  severity={item.severity}
                  hideLabel
                  className="size-3.5 shrink-0"
                />
                <div className="min-w-0 flex-1 basis-64">
                  <Link
                    to={detailPath}
                    className="block truncate text-body-sm font-medium text-text transition-colors hover:text-accent"
                  >
                    {item.title || "（无标题）"}
                  </Link>
                  <p className="mt-0.5 flex flex-wrap items-center gap-x-2 text-caption text-text-muted">
                    <span className="truncate font-mono" title={item.repo}>
                      {item.repo || "—"}
                    </span>
                    <span>{item.reason_text}</span>
                    {item.stuck ? <Badge variant="warning">疑似卡单</Badge> : null}
                    <Badge variant={REASON_VARIANTS[item.reason_code] ?? "neutral"}>
                      {item.reason_code}
                    </Badge>
                  </p>
                </div>
                <StateBadge state={item.state} />
                <span
                  className="inline-flex shrink-0 items-center gap-1 text-caption whitespace-nowrap text-text-muted"
                  title={formatDateTime(item.last_seen_ms)}
                >
                  <Clock3 aria-hidden="true" className="size-3.5" />
                  <time dateTime={isoDateTime(item.last_seen_ms)}>
                    {formatAge(item.last_seen_ms)}
                  </time>
                </span>
                <div className="flex shrink-0 flex-wrap items-center gap-1.5">
                  {item.pr_url ? (
                    <a
                      href={item.pr_url}
                      target="_blank"
                      rel="noreferrer"
                      className="inline-flex items-center gap-1 rounded-sm text-caption text-info transition-colors hover:text-accent"
                    >
                      打开 PR
                      <ExternalLink aria-hidden="true" className="size-3.5" />
                    </a>
                  ) : null}
                  <IssueActionButton
                    fingerprint={item.fingerprint}
                    state={item.state}
                    stuck={item.stuck}
                  />
                  <Link
                    to={detailPath}
                    aria-label={`查看详情：${item.title || item.fingerprint}`}
                    className="inline-flex items-center gap-1 rounded-sm px-1.5 text-caption font-medium whitespace-nowrap text-text-secondary transition-colors hover:text-accent"
                  >
                    查看详情
                    <ArrowRight aria-hidden="true" className="size-3.5" />
                  </Link>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </OverviewSection>
  );
}
