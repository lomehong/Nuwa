import { ChevronRight } from "lucide-react";
import { Link } from "react-router-dom";

import { Badge, Card, CardContent, ISSUE_STATE_VALUES, issueStateLabel } from "../../components/ui";
import { formatNumber } from "./format";
import { OverviewSection } from "./PanelChrome";

interface PipelineFunnelProps {
  counts: Partial<Record<string, number>>;
}

/** Query string consumed by the issue list (Task 5) for a single-state filter. */
export function issuesStatePath(state: string): string {
  return `/issues?state=${encodeURIComponent(state)}`;
}

/**
 * State funnel (docs/09 §10.3.5). Each stage links into the issue list with
 * its `state` filter; arrows keep the reading order explicit and the strip
 * scrolls horizontally on narrow viewports. A proportional bar per chip adds
 * the magnitude channel — "which stage is piling up" is the decision this
 * component supports, and 3 vs 300 should not require reading both numbers.
 */
export function PipelineFunnel({ counts }: PipelineFunnelProps) {
  const total = ISSUE_STATE_VALUES.reduce((sum, state) => sum + (counts[state] ?? 0), 0);
  const max = Math.max(...ISSUE_STATE_VALUES.map((state) => counts[state] ?? 0), 0);

  return (
    <OverviewSection
      id="overview-funnel"
      title="状态漏斗"
      description="点击阶段进入带筛选的问题单列表。"
      aside={<Badge>问题单总数 {formatNumber(total)}</Badge>}
    >
      <Card>
        <CardContent className="p-4">
          <nav aria-label="按状态筛选问题单">
            <ol className="flex items-center gap-1 overflow-x-auto pb-1">
              {ISSUE_STATE_VALUES.map((state, index) => {
                const count = counts[state] ?? 0;
                // Width lives in SVG viewBox units (0-100), not a style
                // attribute: the strict CSP forbids inline styles.
                const width = max > 0 ? Math.max((count / max) * 100, count > 0 ? 4 : 0) : 0;
                return (
                  <li key={state} className="flex shrink-0 items-center gap-1">
                    {index > 0 ? (
                      <ChevronRight
                        aria-hidden="true"
                        className="size-3.5 shrink-0 text-text-muted"
                      />
                    ) : null}
                    <Link
                      to={issuesStatePath(state)}
                      aria-label={`${issueStateLabel(state)}：${formatNumber(count)} 条，查看问题单`}
                      className="inline-flex min-w-20 flex-col items-stretch gap-0.5 rounded-card border border-border bg-raised/40 px-3 py-2 transition-colors hover:border-accent hover:bg-raised"
                    >
                      <span className="text-caption whitespace-nowrap text-text-secondary">
                        {issueStateLabel(state)}
                      </span>
                      <span className="font-mono text-heading-sm font-semibold tabular-nums text-text">
                        {formatNumber(count)}
                      </span>
                      <svg
                        aria-hidden="true"
                        viewBox="0 0 100 4"
                        preserveAspectRatio="none"
                        className="h-1 w-full"
                      >
                        <rect width="100" height="4" className="fill-border" />
                        {width > 0 ? (
                          <rect width={width} height="4" className="fill-accent" />
                        ) : null}
                      </svg>
                    </Link>
                  </li>
                );
              })}
            </ol>
          </nav>
        </CardContent>
      </Card>
    </OverviewSection>
  );
}
