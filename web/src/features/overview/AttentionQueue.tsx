import { AlertTriangle, ArrowRight, Clock3, ExternalLink, ShieldCheck } from "lucide-react";
import { Link } from "react-router-dom";

import {
  Badge,
  Card,
  CardContent,
  EmptyState,
  SeverityStone,
  StateBadge,
  type BadgeVariant,
} from "../../components/ui";
import { IssueActionButton } from "../issues/IssueActionButton";
import { formatAge, formatDateTime, isoDateTime, shortFingerprint } from "./format";
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
        <ul className="grid gap-3 md:grid-cols-2 2xl:grid-cols-3">
          {items.map((item) => {
            const detailPath = issueDetailPath(item.fingerprint);
            return (
              <li key={item.fingerprint} className="min-w-0">
                <Card className="h-full">
                  <CardContent className="flex h-full flex-col gap-3 p-4">
                    <div className="flex flex-wrap items-start justify-between gap-2">
                      <div className="flex flex-wrap items-center gap-2">
                        <SeverityStone severity={item.severity} />
                        <StateBadge state={item.state} />
                        {item.stuck ? <Badge variant="warning">疑似卡单</Badge> : null}
                      </div>
                      <span
                        className="inline-flex items-center gap-1 text-caption whitespace-nowrap text-text-muted"
                        title={formatDateTime(item.last_seen_ms)}
                      >
                        <Clock3 aria-hidden="true" className="size-3.5" />
                        <time dateTime={isoDateTime(item.last_seen_ms)}>
                          {formatAge(item.last_seen_ms)}
                        </time>
                      </span>
                    </div>

                    <div className="min-w-0">
                      <h3 className="text-heading-sm font-medium text-text">
                        <Link
                          to={detailPath}
                          className="rounded-sm transition-colors hover:text-accent"
                        >
                          {item.title || "（无标题）"}
                        </Link>
                      </h3>
                      <p
                        className="mt-1 truncate font-mono text-caption text-text-muted"
                        title={item.fingerprint}
                      >
                        {shortFingerprint(item.fingerprint)}
                      </p>
                    </div>

                    <p className="flex items-start gap-2 text-body-sm text-text-secondary">
                      <AlertTriangle
                        aria-hidden="true"
                        className="mt-0.5 size-4 shrink-0 text-warning"
                      />
                      <span className="min-w-0 flex-1">{item.reason_text}</span>
                      <Badge variant={REASON_VARIANTS[item.reason_code] ?? "neutral"}>
                        {item.reason_code}
                      </Badge>
                    </p>

                    <dl className="mt-auto grid grid-cols-2 gap-x-3 gap-y-1 text-caption">
                      <div className="min-w-0">
                        <dt className="text-text-muted">仓库</dt>
                        <dd className="truncate font-mono text-text-secondary" title={item.repo}>
                          {item.repo || "—"}
                        </dd>
                      </div>
                      <div className="min-w-0">
                        <dt className="text-text-muted">归属运行器</dt>
                        <dd
                          className="truncate font-mono text-text-secondary"
                          title={item.owner_runner_id ?? undefined}
                        >
                          {item.owner_runner_id ?? "未归属"}
                        </dd>
                      </div>
                    </dl>

                    <div className="flex items-center justify-between gap-2 border-t border-border pt-2 text-caption">
                      {item.pr_url ? (
                        <a
                          href={item.pr_url}
                          target="_blank"
                          rel="noreferrer"
                          className="inline-flex items-center gap-1 rounded-sm text-info transition-colors hover:text-accent"
                        >
                          打开 PR
                          <ExternalLink aria-hidden="true" className="size-3.5" />
                        </a>
                      ) : (
                        <span aria-hidden="true" />
                      )}
                      <div className="flex shrink-0 items-center gap-1">
                        <IssueActionButton
                          fingerprint={item.fingerprint}
                          state={item.state}
                          stuck={item.stuck}
                        />
                        <Link
                          to={detailPath}
                          aria-label={`查看详情：${item.title || item.fingerprint}`}
                          className="inline-flex items-center gap-1 rounded-sm px-1.5 font-medium text-text-secondary transition-colors hover:text-accent"
                        >
                          查看详情
                          <ArrowRight aria-hidden="true" className="size-3.5" />
                        </Link>
                      </div>
                    </div>
                  </CardContent>
                </Card>
              </li>
            );
          })}
        </ul>
      )}
    </OverviewSection>
  );
}
