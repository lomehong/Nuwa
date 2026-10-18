import { Activity, ArrowRight } from "lucide-react";
import { Link } from "react-router-dom";

import {
  Badge,
  Card,
  CardContent,
  EmptyState,
  Skeleton,
  type BadgeVariant,
} from "../../components/ui";
import { issueDetailPath } from "./AttentionQueue";
import {
  eventKindLabel,
  formatAge,
  formatDateTime,
  isSystemFingerprint,
  isoDateTime,
  shortFingerprint,
} from "./format";
import { OverviewSection, PanelError } from "./PanelChrome";
import type { EventsResponse } from "../events/types";

const KIND_VARIANTS: Record<string, BadgeVariant> = {
  ci_green: "success",
  pr_merged: "success",
  merged: "success",
  ci_red: "danger",
  runner_error: "danger",
  ci_timeout: "warning",
  restart_requested: "warning",
  pr_opened: "info",
  state_changed: "info",
};

const SKELETON_ROWS = 5;

interface RecentEventsProps {
  events: EventsResponse | undefined;
  isPending: boolean;
  error: unknown;
  onRetry: () => void;
}

/**
 * "最近活动" (docs/09 §10.3.6): the newest lifecycle events. Issue events link
 * to their detail; `__config__` / `__system__` / `__runner__:<id>` rows are
 * labelled as system events because they have no issue page.
 */
export function RecentEvents({ events, isPending, error, onRetry }: RecentEventsProps) {
  const rows = events?.data ?? [];

  return (
    <OverviewSection
      id="overview-events"
      title="最近活动"
      description="生命周期事件与系统审计记录。"
      aside={
        <>
          <Badge>最新 8 条</Badge>
          <Link
            to="/events"
            className="inline-flex min-h-11 items-center gap-1 rounded-card px-2 text-caption font-medium text-text-secondary transition-colors hover:text-accent pointer-fine:min-h-8"
          >
            查看全部
            <ArrowRight aria-hidden="true" className="size-3.5" />
          </Link>
        </>
      }
    >
      <Card>
        <CardContent className="p-4">
          {isPending ? (
            <div role="status" aria-busy="true" className="space-y-2">
              <span className="sr-only">正在加载最近活动</span>
              {Array.from({ length: SKELETON_ROWS }, (_, index) => (
                <div key={index} className="flex items-center gap-2">
                  <Skeleton rounded="full" className="h-5 w-16" />
                  <Skeleton className="h-5 flex-1" />
                  <Skeleton className="h-5 w-16" />
                </div>
              ))}
            </div>
          ) : !events ? (
            <PanelError title="无法加载最近活动" error={error} onRetry={onRetry} compact />
          ) : rows.length === 0 ? (
            <EmptyState
              icon={Activity}
              title="暂无活动记录"
              description="问题单上报、状态迁移、PR 与 CI 事件会按时间倒序出现在这里。"
            />
          ) : (
            <>
              {error ? (
                <p role="status" className="mb-2 text-caption text-warning">
                  活动更新失败，显示的是上次数据。
                </p>
              ) : null}
              <ul className="divide-y divide-border">
                {rows.map((event, index) => {
                  const system = isSystemFingerprint(event.fingerprint);
                  return (
                    <li
                      key={`${event.fingerprint}-${event.kind}-${event.ts_ms}-${index}`}
                      className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2 first:pt-0 last:pb-0"
                    >
                      <Badge
                        variant={KIND_VARIANTS[event.kind] ?? "neutral"}
                        title={event.kind}
                        className="shrink-0"
                      >
                        {eventKindLabel(event.kind)}
                      </Badge>
                      <p
                        className="min-w-0 flex-1 truncate text-body-sm text-text-secondary"
                        title={event.detail}
                      >
                        {event.detail || "—"}
                      </p>
                      {event.count > 1 ? (
                        <span className="shrink-0 font-mono text-caption text-text-muted">
                          ×{event.count}
                        </span>
                      ) : null}
                      {system ? (
                        <span
                          className="shrink-0 text-caption text-text-muted"
                          title={event.fingerprint}
                        >
                          <span>系统</span>
                          <span className="sr-only">（{event.fingerprint}）</span>
                        </span>
                      ) : (
                        <Link
                          to={issueDetailPath(event.fingerprint)}
                          className="shrink-0 rounded-sm font-mono text-caption text-text-secondary transition-colors hover:text-accent"
                          title={event.fingerprint}
                        >
                          {shortFingerprint(event.fingerprint)}
                        </Link>
                      )}
                      <time
                        dateTime={isoDateTime(event.ts_ms)}
                        title={formatDateTime(event.ts_ms)}
                        className="shrink-0 text-caption text-text-muted"
                      >
                        {formatAge(event.ts_ms)}
                      </time>
                    </li>
                  );
                })}
              </ul>
            </>
          )}
        </CardContent>
      </Card>
    </OverviewSection>
  );
}
