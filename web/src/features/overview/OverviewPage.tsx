import { RefreshCw } from "lucide-react";
import { useState } from "react";

import { Button, Skeleton } from "../../components/ui";
import { ActiveRuns } from "./ActiveRuns";
import { AttentionQueue } from "./AttentionQueue";
import { HealthPanel } from "./HealthPanel";
import { PanelError } from "./PanelChrome";
import { PipelineFunnel } from "./PipelineFunnel";
import { useOverviewQuery, useRecentEventsQuery, useTrendsQuery } from "./queries";
import { RecentEvents } from "./RecentEvents";
import { TrendsPanel } from "./TrendsPanel";
import type { TrendsRange } from "./types";

function CoreSkeleton() {
  return (
    <div role="status" aria-busy="true" className="space-y-6">
      <span className="sr-only">正在加载总览</span>
      {["需要你处理", "正在发生", "系统健康"].map((label) => (
        <div key={label} className="space-y-3">
          <Skeleton className="h-6 w-32" />
          <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
            <Skeleton className="h-28" />
            <Skeleton className="h-28" />
            <Skeleton className="h-28 xl:block" />
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * Console overview (docs/09 §10.3). Priority order: needs-attention queue,
 * live runs, system health, trends, state funnel, recent events.
 *
 * `/overview` drives attention, runs, health and the funnel; trends and events
 * load independently so a failure of one read endpoint never blanks the page.
 * The always-mounted shell health strip owns the single 10s `/overview`
 * interval (this observer shares the key but passes `refetchInterval: false`);
 * trends and events poll at 60s/30s. All pause while hidden and refresh on
 * focus (see `queries.ts`).
 */
export function OverviewPage() {
  const [range, setRange] = useState<TrendsRange>("14d");
  const overview = useOverviewQuery(null, { refetchInterval: false });
  const trends = useTrendsQuery(range);
  const events = useRecentEventsQuery();

  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-title font-semibold text-text">总览</h2>
        <p className="mt-1 text-body-sm text-text-secondary">
          需要你处理的事项、正在发生的运行、系统健康与可按 7/14/30/90 天切换的趋势。
        </p>
        <p className="mt-1 text-caption text-text-muted">
          核心数据每 10 秒自动刷新，趋势每 60 秒、最近活动每 30 秒；窗口重新聚焦时立即更新。
        </p>
      </header>

      {overview.data ? (
        <>
          {overview.isError ? (
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text">
              {/* Only the message is a live region; the retry button stays an
                  ordinary control outside it. */}
              <span role="status">总览更新失败，正在显示上次数据。</span>
              <Button variant="ghost" size="sm" onClick={() => void overview.refetch()}>
                <RefreshCw aria-hidden="true" className="size-3.5" />
                重试
              </Button>
            </div>
          ) : null}
          <AttentionQueue items={overview.data.attention} />
          <ActiveRuns runs={overview.data.active_runs} queue={overview.data.queue} />
          <HealthPanel health={overview.data.health} ci={overview.data.ci_24h} />
        </>
      ) : overview.isPending ? (
        <CoreSkeleton />
      ) : (
        <PanelError
          title="无法加载总览"
          error={overview.error}
          onRetry={() => void overview.refetch()}
        />
      )}

      <TrendsPanel
        trends={trends.data}
        isPending={trends.isPending}
        error={trends.error}
        onRetry={() => void trends.refetch()}
        range={range}
        onRangeChange={setRange}
      />

      {overview.data ? <PipelineFunnel counts={overview.data.counts} /> : null}

      <RecentEvents
        events={events.data}
        isPending={events.isPending}
        error={events.error}
        onRetry={() => void events.refetch()}
      />
    </div>
  );
}
