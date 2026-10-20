import { clsx } from "clsx";
import { ArrowLeft, ExternalLink, FolderTree } from "lucide-react";
import { Link, useParams } from "react-router-dom";

import { Sparkline, type SparklineDatum } from "../../components/charts/Sparkline";
import {
  Card,
  CardContent,
  EmptyState,
  SeverityStone,
  Skeleton,
  StateBadge,
} from "../../components/ui";
import { eventKindLabel } from "../../lib/eventKinds";
import { DEFAULT_EVENT_FILTERS } from "../events/filters";
import { useEventsQuery } from "../events/queries";
import { useIssuesQuery } from "../issues/queries";
import { DEFAULT_ISSUE_FILTERS } from "../issues/types";
import { formatAge, formatNumber, formatPercent, formatSeconds, formatTokens } from "../overview/format";
import { useOverviewQuery, useTrendsQuery } from "../overview/queries";
import { useRunnersQuery } from "../runners/queries";
import { formatLastSignal, projectLabel } from "./format";
import { useProjectsQuery } from "./queries";

function LedgerStat({
  label,
  value,
  tone,
}: {
  label: string;
  value: string;
  tone?: "danger";
}) {
  return (
    <div className="min-w-0 px-3 py-2">
      <p className="truncate text-caption text-text-muted">{label}</p>
      <p
        className={clsx(
          "ledger__num mt-0.5 text-body font-medium",
          tone === "danger" ? "text-danger" : "text-text",
        )}
      >
        {value}
      </p>
    </div>
  );
}

/**
 * 项目工位 — one project's workbench: what it needs, what is running, what
 * the runner and PR quota look like, and how it has been trending.
 */
export function ProjectPage() {
  const { repo = "" } = useParams<{ repo: string }>();
  const decoded = decodeURIComponent(repo);
  const projects = useProjectsQuery();
  const overview = useOverviewQuery(decoded);
  const trends = useTrendsQuery("14d", decoded);
  const runners = useRunnersQuery();
  const issues = useIssuesQuery({
    ...DEFAULT_ISSUE_FILTERS,
    repo: decoded,
    pageSize: 8,
    sort: "last_seen",
    order: "desc",
  });
  const events = useEventsQuery({
    ...DEFAULT_EVENT_FILTERS,
    repo: decoded,
    range: "7d",
  });

  const project = projects.data?.data.find((entry) => entry.repo === decoded);

  if (projects.isPending) {
    return (
      <div className="space-y-4" role="status" aria-busy="true">
        <span className="sr-only">正在加载项目</span>
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-32 w-full" />
      </div>
    );
  }

  if (!project) {
    return (
      <Card>
        <CardContent className="p-6">
          <EmptyState
            icon={FolderTree}
            title="找不到这个项目"
            description={`没有名为 ${decoded} 的项目。它可能还没有上报过信号。`}
          />
        </CardContent>
      </Card>
    );
  }

  const label = projectLabel(project.name, project.repo);
  const projectRunners = (runners.data?.data ?? []).filter(
    (runner) => runner.repo === project.repo,
  );
  const attention = overview.data?.attention ?? [];
  const totals = trends.data?.totals;
  const buckets = trends.data?.buckets ?? [];

  const sparkline = (metric: "reports" | "new_issues" | "prs_opened"): SparklineDatum[] =>
    buckets.map((bucket) => ({ label: bucket.day, value: bucket[metric] }));

  return (
    <section className="space-y-5">
      <header className="space-y-3">
        <Link
          to="/projects"
          className="inline-flex min-h-11 items-center gap-1.5 text-body-sm text-text-secondary transition-colors hover:text-accent"
        >
          <ArrowLeft aria-hidden="true" className="size-4" />
          项目台账
        </Link>
        <div className="flex flex-wrap items-start gap-3">
          {project.worst_severity ? (
            <SeverityStone
              severity={project.worst_severity}
              hideLabel
              className="mt-1 size-5 shrink-0"
            />
          ) : null}
          <div className="min-w-0 flex-1">
            <h2 className="text-title font-semibold tracking-tight break-words text-text">
              {label}
            </h2>
            <p className="mt-0.5 flex flex-wrap items-center gap-2 font-mono text-caption text-text-muted">
              <span className="break-all">{project.repo}</span>
              {project.url ? (
                <a
                  href={project.url}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1 text-text-secondary transition-colors hover:text-accent"
                >
                  <ExternalLink aria-hidden="true" className="size-3" />
                  仓库
                </a>
              ) : null}
              {!project.registered ? (
                <span className="text-caption text-text-muted">未登记</span>
              ) : null}
            </p>
            {project.description ? (
              <p className="mt-1 text-body-sm text-text-secondary">{project.description}</p>
            ) : null}
          </div>
          <div className="text-right">
            <p className="text-caption text-text-muted">最近信号</p>
            <p className="text-body-sm text-text-secondary">
              {formatLastSignal(project.last_signal_ms)}
            </p>
          </div>
        </div>
      </header>

      <div className="ledger grid grid-cols-2 divide-border rounded-card border border-border bg-surface sm:grid-cols-4 lg:grid-cols-8">
        <LedgerStat
          label="需处理"
          value={String(project.attention)}
          tone={project.attention > 0 ? "danger" : undefined}
        />
        <LedgerStat label="队列" value={String(project.queued)} />
        <LedgerStat label="运行中" value={String(project.running)} />
        <LedgerStat label="PR 待合并" value={String(project.pr_open)} />
        <LedgerStat label="今日 PR" value={`${project.prs_today}/${project.pr_limit || "—"}`} />
        <LedgerStat
          label="CI 红 24h"
          value={String(project.ci_red)}
          tone={project.ci_red > 0 ? "danger" : undefined}
        />
        <LedgerStat label="上报 24h" value={formatNumber(project.reports_24h)} />
        <LedgerStat label="tokens 24h" value={formatTokens(project.tokens_24h)} />
      </div>

      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <div className="space-y-5">
          <Card>
            <CardContent className="p-4">
              <header className="mb-2 flex items-center justify-between gap-2">
                <h3 className="text-heading-sm font-semibold text-text">需要你处理</h3>
                <Link
                  to={`/issues?attention=1&repo=${encodeURIComponent(project.repo)}`}
                  className="text-caption text-text-secondary transition-colors hover:text-accent"
                >
                  全部
                </Link>
              </header>
              {attention.length === 0 ? (
                <p className="py-3 text-body-sm text-text-muted">
                  这个项目现在没有需要你处理的事项。新的信号到来时会自动进入队列。
                </p>
              ) : (
                <ul className="ledger divide-y divide-border">
                  {attention.map((item) => (
                    <li key={item.fingerprint} className="ledger__row flex items-center gap-3 py-2">
                      <SeverityStone severity={item.severity} hideLabel className="size-3.5 shrink-0" />
                      <div className="min-w-0 flex-1">
                        <Link
                          to={`/issues/${encodeURIComponent(item.fingerprint)}`}
                          className="block truncate text-body-sm text-text transition-colors hover:text-accent"
                        >
                          {item.title}
                        </Link>
                        <p className="truncate text-caption text-text-muted">{item.reason_text}</p>
                      </div>
                      <StateBadge state={item.state} />
                      <span className="ledger__num shrink-0 text-caption text-text-muted">
                        {formatAge(item.last_seen_ms)}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardContent className="p-4">
              <header className="mb-2 flex items-center justify-between gap-2">
                <h3 className="text-heading-sm font-semibold text-text">问题单</h3>
                <Link
                  to={`/issues?repo=${encodeURIComponent(project.repo)}`}
                  className="text-caption text-text-secondary transition-colors hover:text-accent"
                >
                  全部
                </Link>
              </header>
              {issues.data?.data.length ? (
                <ul className="ledger divide-y divide-border">
                  {issues.data.data.map((issue) => (
                    <li key={issue.fingerprint} className="ledger__row flex items-center gap-3 py-2">
                      <SeverityStone severity={issue.severity} hideLabel className="size-3.5 shrink-0" />
                      <Link
                        to={`/issues/${encodeURIComponent(issue.fingerprint)}`}
                        className="min-w-0 flex-1 truncate text-body-sm text-text transition-colors hover:text-accent"
                      >
                        {issue.title}
                      </Link>
                      {issue.attempts ? (
                        <span className="ledger__num shrink-0 text-caption text-text-muted">
                          尝试 {issue.attempts}
                        </span>
                      ) : null}
                      <StateBadge state={issue.state} />
                      <span className="ledger__num w-16 shrink-0 text-right text-caption text-text-muted">
                        {formatAge(issue.last_seen_ms)}
                      </span>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="py-3 text-body-sm text-text-muted">这个项目没有未闭环的问题单。</p>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardContent className="p-4">
              <header className="mb-2 flex items-center justify-between gap-2">
                <h3 className="text-heading-sm font-semibold text-text">最近活动</h3>
                <Link
                  to={`/events?repo=${encodeURIComponent(project.repo)}`}
                  className="text-caption text-text-secondary transition-colors hover:text-accent"
                >
                  全部
                </Link>
              </header>
              {events.data?.data.length ? (
                <ul className="ledger divide-y divide-border">
                  {events.data.data.slice(0, 8).map((event, index) => (
                    <li
                      key={`${event.fingerprint}-${event.ts_ms}-${index}`}
                      className="ledger__row flex items-center gap-3 py-1.5"
                    >
                      <span className="shrink-0 text-caption text-text-secondary">
                        {eventKindLabel(event.kind)}
                      </span>
                      <span className="min-w-0 flex-1 truncate text-body-sm text-text-secondary">
                        {event.detail}
                      </span>
                      <span className="ledger__num shrink-0 text-caption text-text-muted">
                        {formatAge(event.ts_ms)}
                      </span>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="py-3 text-body-sm text-text-muted">最近 7 天没有活动记录。</p>
              )}
            </CardContent>
          </Card>
        </div>

        <div className="space-y-5">
          <Card>
            <CardContent className="p-4">
              <h3 className="mb-2 text-heading-sm font-semibold text-text">运行器</h3>
              {projectRunners.length === 0 ? (
                <p className="py-3 text-body-sm text-text-muted">
                  这个项目还没有 runner 上报心跳。
                </p>
              ) : (
                <ul className="ledger divide-y divide-border">
                  {projectRunners.map((runner) => (
                    <li key={runner.id} className="ledger__row py-2">
                      <p className="truncate font-mono text-caption text-text">{runner.id}</p>
                      <p className="mt-0.5 text-caption text-text-muted">
                        {runner.status}
                        {runner.phase ? ` · ${runner.phase}` : ""} ·{" "}
                        {formatAge(runner.heartbeat_at_ms)}
                      </p>
                    </li>
                  ))}
                </ul>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardContent className="p-4">
              <h3 className="mb-2 text-heading-sm font-semibold text-text">交付质量</h3>
              <dl className="space-y-2">
                <div className="flex items-baseline justify-between gap-2">
                  <dt className="text-caption text-text-muted">误报 PR 率</dt>
                  <dd className="ledger__num text-body-sm text-text">
                    {formatPercent(totals?.false_positive_rate ?? null)}
                  </dd>
                </div>
                <div className="flex items-baseline justify-between gap-2">
                  <dt className="text-caption text-text-muted">Review 延迟中位数</dt>
                  <dd className="ledger__num text-body-sm text-text">
                    {formatSeconds(totals?.review_latency_p50_secs ?? null)}
                  </dd>
                </div>
                <div className="flex items-baseline justify-between gap-2">
                  <dt className="text-caption text-text-muted">CI 红率</dt>
                  <dd className="ledger__num text-body-sm text-text">
                    {formatPercent(totals?.ci_red_rate ?? null)}
                  </dd>
                </div>
              </dl>
            </CardContent>
          </Card>

          <Card>
            <CardContent className="p-4">
              <h3 className="mb-2 text-heading-sm font-semibold text-text">14 天趋势</h3>
              <div className="space-y-3">
                <div>
                  <p className="text-caption text-text-muted">上报</p>
                  <Sparkline
                    data={sparkline("reports")}
                    label="上报"
                    valueFormatter={formatNumber}
                    emptyLabel="暂无数据"
                  />
                </div>
                <div>
                  <p className="text-caption text-text-muted">新单</p>
                  <Sparkline
                    data={sparkline("new_issues")}
                    label="新单"
                    valueFormatter={formatNumber}
                    emptyLabel="暂无数据"
                  />
                </div>
                <div>
                  <p className="text-caption text-text-muted">打开 PR</p>
                  <Sparkline
                    data={sparkline("prs_opened")}
                    label="打开 PR"
                    valueFormatter={formatNumber}
                    emptyLabel="暂无数据"
                  />
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </section>
  );
}
