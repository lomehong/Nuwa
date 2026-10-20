import { clsx } from "clsx";
import { ChartColumn } from "lucide-react";

import { Card, CardContent, EmptyState, Skeleton } from "../../components/ui";
import { Sparkline, type SparklineDatum } from "../../components/charts/Sparkline";
import {
  formatDayShort,
  formatNumber,
  formatPercent,
  formatSeconds,
  formatTokens,
} from "./format";
import { OverviewSection, PanelError } from "./PanelChrome";
import type { TrendBucket, TrendTotals, TrendsRange, TrendsResponse } from "./types";

interface TrendMetric {
  key: keyof Pick<
    TrendBucket,
    "reports" | "new_issues" | "prs_opened" | "tokens"
  >;
  label: string;
  format: (value: number) => string;
}

const TREND_METRICS: readonly TrendMetric[] = [
  { key: "reports", label: "上报", format: formatNumber },
  { key: "new_issues", label: "新单", format: formatNumber },
  { key: "prs_opened", label: "打开 PR", format: formatNumber },
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

const TREND_RANGES: readonly { value: TrendsRange; label: string; days: number }[] = [
  { value: "7d", label: "7 天", days: 7 },
  { value: "14d", label: "14 天", days: 14 },
  { value: "30d", label: "30 天", days: 30 },
  { value: "90d", label: "90 天", days: 90 },
];

/** URL guard for the `?range=` param (docs/09 §10.3.4, range is linkable). */
export function isTrendsRange(value: string | null): value is TrendsRange {
  return TREND_RANGES.some((entry) => entry.value === value);
}

function trendRangeDays(range: TrendsRange): number {
  return TREND_RANGES.find((entry) => entry.value === range)?.days ?? 14;
}

/**
 * Direction read: the later half of the window vs the earlier half. The API
 * exposes no offset query, and this stays within already-fetched buckets —
 * it answers "变好还是变坏" without a second request.
 */
function halfSplit(
  buckets: TrendBucket[],
  key: TrendMetric["key"],
): { earlier: number; later: number } | null {
  const n = buckets.length;
  if (n < 4) {
    return null;
  }
  const half = Math.floor(n / 2);
  const sum = (rows: TrendBucket[]) =>
    rows.reduce((acc, bucket) => acc + (bucket[key] ?? 0), 0);
  return { earlier: sum(buckets.slice(0, half)), later: sum(buckets.slice(n - half)) };
}

function HalfDelta({ earlier, later }: { earlier: number; later: number }) {
  if (earlier === 0 && later === 0) {
    return null;
  }
  let text: string;
  if (earlier === 0) {
    text = "后半程新增";
  } else {
    const pct = Math.round(((later - earlier) / earlier) * 100);
    text = pct === 0 ? "持平" : `${pct > 0 ? "▲" : "▼"} ${Math.abs(pct)}%`;
  }
  return (
    <span
      className="text-caption font-normal tabular-nums text-text-muted"
      title={`后半程合计 ${later}，前半程合计 ${earlier}`}
    >
      {text}
    </span>
  );
}

interface TrendsPanelProps {
  trends: TrendsResponse | undefined;
  isPending: boolean;
  error: unknown;
  onRetry: () => void;
  range: TrendsRange;
  onRangeChange: (range: TrendsRange) => void;
}

/**
 * "趋势" (docs/09 §10.3.4): 7/14/30/90 UTC daily buckets, one bar sparkline
 * per metric plus the range total. The API zero-fills missing days, but an
 * empty bucket list is handled explicitly so a fresh database never renders
 * NaN geometry.
 */
export function TrendsPanel({
  trends,
  isPending,
  error,
  onRetry,
  range,
  onRangeChange,
}: TrendsPanelProps) {
  const days = trendRangeDays(range);

  return (
    <OverviewSection
      id="overview-trends"
      title="趋势"
      description={`最近 ${days} 天 · 按 UTC 日分桶`}
      aside={
        <div
          role="group"
          aria-label="趋势时间范围"
          className="inline-flex items-center rounded-full border border-border bg-surface p-0.5"
        >
          {TREND_RANGES.map((entry) => {
            const pressed = entry.value === range;
            return (
              <button
                key={entry.value}
                type="button"
                aria-pressed={pressed}
                onClick={() => onRangeChange(entry.value)}
                className={clsx(
                  "inline-flex min-h-11 items-center rounded-full px-3 text-caption font-medium",
                  "transition-colors duration-150 ease-out pointer-fine:min-h-8",
                  pressed
                    ? "bg-accent/10 text-text"
                    : "text-text-secondary hover:text-text",
                )}
              >
                {entry.label}
              </button>
            );
          })}
        </div>
      }
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
              description={`最近 ${days} 天还没有记录；新的上报、PR 与 CI 结果会按 UTC 日汇总在这里。`}
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
                  const split = halfSplit(trends.buckets, metric.key);
                  return (
                    <li
                      key={metric.key}
                      className="grid grid-cols-[5.5rem_1fr] items-center gap-3 py-2 first:pt-0 last:pb-0"
                    >
                      <div className="min-w-0">
                        <p className="truncate text-body-sm text-text-secondary">{metric.label}</p>
                        <p className="flex flex-wrap items-baseline gap-x-1.5 font-mono text-body font-semibold tabular-nums text-text">
                          {metric.format(totalsFor(trends.totals, metric.key))}
                          {split ? (
                            <HalfDelta earlier={split.earlier} later={split.later} />
                          ) : null}
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
              <dl className="mt-4 grid grid-cols-1 gap-3 border-t border-border pt-3 text-caption sm:grid-cols-3">
                <div>
                  <dt className="text-text-muted">误报 PR 率</dt>
                  <dd className="mt-0.5 font-mono text-body text-text">
                    {formatPercent(trends.totals.false_positive_rate)}
                  </dd>
                  <p className="text-caption text-text-muted">拒绝 /（合并 + 拒绝）</p>
                </div>
                <div>
                  <dt className="text-text-muted">Review 延迟中位数</dt>
                  <dd className="mt-0.5 font-mono text-body text-text">
                    {formatSeconds(trends.totals.review_latency_p50_secs)}
                  </dd>
                  <p className="text-caption text-text-muted">PR 打开 → 合并/拒绝</p>
                </div>
                <div>
                  <dt className="text-text-muted">CI 红率</dt>
                  <dd className="mt-0.5 font-mono text-body text-text">
                    {formatPercent(trends.totals.ci_red_rate)}
                  </dd>
                  <p className="text-caption text-text-muted">红 /（绿 + 红）</p>
                </div>
              </dl>
            </>
          )}
        </CardContent>
      </Card>
    </OverviewSection>
  );
}
