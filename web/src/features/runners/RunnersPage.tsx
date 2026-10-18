import { Cpu } from "lucide-react";

import { Card, CardContent, EmptyState, Skeleton } from "../../components/ui";
import { describeError, PanelError } from "../overview/PanelChrome";
import { runnerIsOffline } from "./format";
import { useRunnersQuery } from "./queries";
import { RunnerCard } from "./RunnerCard";
import type { RunnerDto } from "./types";

interface SummaryItem {
  key: string;
  label: string;
  value: string;
}

/**
 * Fleet counts. Like the status badges, only runners with a live heartbeat
 * contribute; an offline runner keeps its last status in the card but must not
 * inflate "working" or "error" in the strip.
 */
function fleetSummary(runners: RunnerDto[], nowMs: number): SummaryItem[] {
  const online = runners.filter((runner) => !runnerIsOffline(runner, nowMs));
  const count = (status: string) =>
    online.filter((runner) => runner.status === status).length;
  return [
    { key: "online", label: "在线", value: `${online.length}/${runners.length}` },
    { key: "working", label: "工作中", value: String(count("working")) },
    { key: "idle", label: "待命", value: String(count("idle")) },
    { key: "standing-down", label: "配额待命", value: String(count("standing_down")) },
    { key: "error", label: "异常", value: String(count("error")) },
  ];
}

function RunnerSummary({ runners, nowMs }: { runners: RunnerDto[]; nowMs: number }) {
  return (
    <section aria-label="运行器概览" data-testid="runner-summary">
      <Card>
        <CardContent className="p-4">
          <dl className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
            {fleetSummary(runners, nowMs).map((item) => (
              <div
                key={item.key}
                data-testid={`runner-summary-${item.key}`}
                className="rounded-card bg-raised px-3 py-2"
              >
                <dt className="text-caption text-text-muted">{item.label}</dt>
                <dd className="mt-0.5 font-mono text-heading-sm tabular-nums text-text">
                  {item.value}
                </dd>
              </div>
            ))}
          </dl>
        </CardContent>
      </Card>
    </section>
  );
}

function RunnersSkeleton() {
  return (
    <div role="status" aria-busy="true">
      <span className="sr-only">正在加载运行器</span>
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        {Array.from({ length: 3 }, (_, index) => (
          <Card key={index}>
            <CardContent className="space-y-3 p-5">
              <div className="flex items-center gap-2">
                <Skeleton rounded="full" className="size-9 shrink-0" />
                <Skeleton className="h-4 w-32" />
              </div>
              <Skeleton className="h-3 w-2/3" />
              <Skeleton className="h-16 w-full" />
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-2 w-full" />
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  );
}

function RunnersEmpty() {
  return (
    <EmptyState
      icon={Cpu}
      title="还没有运行器注册"
      description="在目标主机上运行 nuwa work 并指定唯一的 runner id，第一拍心跳上报后这里会出现运行器卡片。"
      action={
        <pre className="mx-auto max-w-full overflow-x-auto rounded-card border border-border bg-raised px-3 py-2 text-left font-mono text-caption text-text-secondary">
          <code>{"nuwa work --loop --runner-id <host:repo> --provider <provider>"}</code>
        </pre>
      }
    />
  );
}

/**
 * Runner fleet (docs/09 §10.5). The roster is read-only in M4; each card shows
 * liveness, current issue, quota and the last error, and the page polls every
 * 10s so heartbeat ages and statuses stay honest.
 */
export function RunnersPage() {
  const query = useRunnersQuery();
  const nowMs = Date.now();
  const data = query.data;
  const runners = data?.data ?? [];
  const isInitialLoading = query.isPending && !data;

  return (
    <div className="space-y-4">
      <header>
        <h2 className="text-title font-semibold text-text">运行器</h2>
        <p className="mt-1 text-body-sm text-text-secondary">
          每台注册工作机的在线状态、当前任务、日配额与最近错误。
        </p>
        <p className="mt-1 text-caption text-text-muted">
          每 10 秒自动刷新，标签页隐藏时暂停；窗口重新聚焦时立即更新。
        </p>
      </header>

      {isInitialLoading ? <RunnersSkeleton /> : null}

      {query.isError && !data ? (
        <PanelError
          title="无法加载运行器"
          error={query.error}
          onRetry={() => void query.refetch()}
        />
      ) : null}

      {data && runners.length > 0 ? <RunnerSummary runners={runners} nowMs={nowMs} /> : null}

      {query.isError && data ? (
        <p
          role="status"
          className="rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text"
        >
          运行器状态更新失败，显示的是上次数据。
          <span className="ml-1 text-text-secondary">{describeError(query.error)}</span>
        </p>
      ) : null}

      {data && runners.length === 0 && !query.isError ? <RunnersEmpty /> : null}

      {runners.length > 0 ? (
        <div
          className="grid gap-4 md:grid-cols-2 xl:grid-cols-3"
          aria-busy={query.isFetching || undefined}
        >
          {runners.map((runner) => (
            <RunnerCard key={runner.id} runner={runner} nowMs={nowMs} />
          ))}
        </div>
      ) : null}
    </div>
  );
}
