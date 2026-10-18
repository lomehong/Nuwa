import { ChartColumn } from "lucide-react";

import { Badge, Card, CardContent, EmptyState, Skeleton } from "../../components/ui";
import { Sparkline, type SparklineDatum } from "../../components/charts/Sparkline";
import { formatDayShort, formatNumber, formatTokens } from "./format";
import { OverviewSection, PanelError } from "./PanelChrome";
import type { TrendBucket, TrendTotals, TrendsResponse } from "./types";

interface TrendMetric {
  key: keyof Pick<
    TrendBucket,
    "reports" | "new_issues" | "prs_opened" | "merged" | "rejected" | "tokens"
  >;
  label: string;
  format: (value: number) => string;
}

const TREND_METRICS: readonly TrendMetric[] = [
  { key: "reports", label: "上报", format: formatNumber },
  { key: "new_issues", label: "新单", format: formatNumber },
  { key: "prs_opened", label: "打开 PR", format: formatNumber },
  { key: "merged", label: "合并", format: formatNumber },
  { key: "rejected", label: "拒绝", format: formatNumber },
  { key: "tokens", label: "tokens", format: formatTokens },
];

function totalsFor(totals: TrendTotals | undefined, key: TrendMetric["key"]): number {
  return totals?.[key] ?? 0;
}

function TrendSkeleton() {
  return (
    <div role="status" aria-busy="true" className="space-y-3">
      <span className="sr-only">正在加载趋势</span>
      {TREND_METRICS.map((metric) => (
        <div key={metric.key} className="grid grid-cols-[5.5rem_1fr] items-center gap-3">
          <Skeleton className="h-8 w-16" />
          <Skeleton className="h-9 w-full" />
        </div>
      ))}
    </div>
  );
}

interface TrendsPanelProps {
  trends: TrendsResponse | undefined;
  isPending: boolean;
  error: unknown;
  onRetry: () => void;
}

/**
 * "趋势" (docs/09 §10.3.4): 14 UTC daily buckets, one bar sparkline per metric
 * plus the range total. The API zero-fills missing days, but an empty bucket
 * list is handled explicitly so a fresh database never renders NaN geometry.
 */
export function TrendsPanel({ trends, isPending, error, onRetry }: TrendsPanelProps) {
  return (
    <OverviewSection
      id="overview-trends"
      title="趋势"
      description="最近 14 天 · 按 UTC 日分桶"
      aside={<Badge>14 天</Badge>}
    >
      <Card>
        <CardContent className="p-4">
          {isPending ? (
            <TrendSkeleton />
          ) : !trends ? (
            <PanelError title="无法加载趋势" error={error} onRetry={onRetry} compact />
          ) : trends.buckets.length === 0 ? (
            <EmptyState
              icon={ChartColumn}
              title="暂无趋势数据"
              description="最近 14 天还没有记录；新的上报、PR 与 CI 结果会按 UTC 日汇总在这里。"
            />
          ) : (
            <>
              {error ? (
                <p role="status" className="mb-3 text-caption text-warning">
                  趋势更新失败，显示的是上次数据。
                </p>
              ) : null}
              <ul className="divide-y divide-border">
                {TREND_METRICS.map((metric) => {
                  const data: SparklineDatum[] = trends.buckets.map((bucket) => ({
                    label: formatDayShort(bucket.day),
                    value: bucket[metric.key],
                  }));
                  return (
                    <li
                      key={metric.key}
                      className="grid grid-cols-[5.5rem_1fr] items-center gap-3 py-2 first:pt-0 last:pb-0"
                    >
                      <div className="min-w-0">
                        <p className="truncate text-body-sm text-text-secondary">{metric.label}</p>
                        <p className="font-mono text-body font-semibold tabular-nums text-text">
                          {metric.format(totalsFor(trends.totals, metric.key))}
                        </p>
                      </div>
                      <Sparkline
                        data={data}
                        label={metric.label}
                        valueFormatter={metric.format}
                        emptyLabel="暂无数据"
                      />
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
