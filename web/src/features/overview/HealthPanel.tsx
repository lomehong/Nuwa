import {
  Activity,
  AlertTriangle,
  ArrowRight,
  CloudOff,
  Coffee,
  FileCheck2,
  GitPullRequest,
  PauseCircle,
  Radio,
  type LucideIcon,
} from "lucide-react";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";

import { Badge, Card, CardContent } from "../../components/ui";
import { formatNumber } from "./format";
import { OverviewSection } from "./PanelChrome";
import type { OverviewCiCounts, OverviewHealth } from "./types";

interface RunnerTile {
  key: string;
  label: string;
  value: ReactNode;
  hint: string;
  icon: LucideIcon;
  iconClass: string;
  needsAttention: boolean;
}

function runnerTiles(health: OverviewHealth): RunnerTile[] {
  const offline = Math.max(0, health.runners_total - health.runners_online);
  return [
    {
      key: "online",
      label: "在线",
      value: `${health.runners_online}/${health.runners_total}`,
      hint: "最近 90 秒内有心跳",
      icon: Radio,
      iconClass: "text-success",
      needsAttention: false,
    },
    {
      key: "working",
      label: "工作中",
      value: formatNumber(health.working),
      hint: "正在修复问题单",
      icon: Activity,
      iconClass: "text-info",
      needsAttention: false,
    },
    {
      key: "idle",
      label: "空闲",
      value: formatNumber(health.idle),
      hint: "在线但队列为空",
      icon: Coffee,
      iconClass: "text-text-secondary",
      needsAttention: false,
    },
    {
      key: "offline",
      label: "离线",
      value: formatNumber(offline),
      hint: "心跳超过 90 秒",
      icon: CloudOff,
      iconClass: "text-warning",
      needsAttention: offline > 0,
    },
    {
      key: "standing_down",
      label: "配额待命",
      value: formatNumber(health.standing_down),
      hint: "今日 PR 配额用完，UTC 次日恢复",
      icon: PauseCircle,
      iconClass: "text-warning",
      needsAttention: health.standing_down > 0,
    },
    {
      key: "error",
      label: "异常",
      value: formatNumber(health.error),
      hint: "最近一次运行报错",
      icon: AlertTriangle,
      iconClass: "text-danger",
      needsAttention: health.error > 0,
    },
  ];
}

interface CiSegment {
  key: keyof OverviewCiCounts;
  label: string;
  fill: string;
  dot: string;
}

/** 24h event counters. `pending` is a current snapshot and sits apart below. */
const CI_24H_SEGMENTS: readonly CiSegment[] = [
  { key: "green", label: "通过", fill: "fill-success", dot: "bg-success" },
  { key: "red", label: "失败", fill: "fill-danger", dot: "bg-danger" },
  { key: "timeout", label: "超时", fill: "fill-warning", dot: "bg-warning" },
];

const CI_PENDING_DOT = "bg-info";

function CiBar({ counts }: { counts: OverviewCiCounts }) {
  const total = CI_24H_SEGMENTS.reduce((sum, segment) => sum + counts[segment.key], 0);
  const summary = CI_24H_SEGMENTS.map(
    (segment) => `${segment.label} ${formatNumber(counts[segment.key])}`,
  ).join("，");
  let offset = 0;

  return (
    <svg
      role="img"
      aria-label={`最近 24 小时 CI：${summary}`}
      viewBox="0 0 100 8"
      preserveAspectRatio="none"
      className="block h-2 w-full overflow-hidden rounded-full bg-raised"
    >
      <title>{`最近 24 小时 CI：${summary}`}</title>
      {total === 0 ? (
        <rect x={0} y={0} width={100} height={8} className="fill-border" />
      ) : (
        CI_24H_SEGMENTS.map((segment) => {
          const width = (counts[segment.key] / total) * 100;
          const x = offset;
          offset += width;
          if (width <= 0) {
            return null;
          }
          return (
            <rect
              key={segment.key}
              x={x}
              y={0}
              width={width}
              height={8}
              className={segment.fill}
            >
              <title>{`${segment.label}：${formatNumber(counts[segment.key])}`}</title>
            </rect>
          );
        })
      )}
    </svg>
  );
}

interface HealthPanelProps {
  health: OverviewHealth;
  ci: OverviewCiCounts;
}

/**
 * "系统健康" (docs/09 §10.3.3): runner tiles distinguish offline (heartbeat
 * timeout), standing-down (quota) and error states in text, icon and colour;
 * CI counts and today's throughput sit underneath.
 */
export function HealthPanel({ health, ci }: HealthPanelProps) {
  const tiles = runnerTiles(health);

  return (
    <OverviewSection
      id="overview-health"
      title="系统健康"
      description="运行器状态、最近 24 小时 CI 结果与今日产出。"
      aside={
        <>
          <Badge>运行器总数 {formatNumber(health.runners_total)}</Badge>
          <Link
            to="/runners"
            className="inline-flex min-h-11 items-center gap-1 rounded-card px-2 text-caption font-medium text-text-secondary transition-colors hover:text-accent pointer-fine:min-h-8"
          >
            查看运行器
            <ArrowRight aria-hidden="true" className="size-3.5" />
          </Link>
        </>
      }
    >
      <div className="space-y-3">
        <ul className="ledger grid grid-cols-2 gap-px overflow-hidden rounded-card border border-border bg-border sm:grid-cols-3 xl:grid-cols-6">
          {tiles.map((tile) => {
            const Icon = tile.icon;
            return (
              <li key={tile.key} className="bg-surface px-3 py-2" data-runner-tile={tile.key}>
                <div className="flex items-center justify-between gap-2">
                  <span className="inline-flex min-w-0 items-center gap-1.5 text-caption text-text-secondary">
                    <Icon aria-hidden="true" className={`size-3.5 shrink-0 ${tile.iconClass}`} />
                    <span className="truncate">{tile.label}</span>
                  </span>
                  <span className="ledger__num shrink-0 text-body-sm font-medium text-text">
                    {tile.value}
                  </span>
                </div>
                <p className="mt-0.5 truncate text-caption text-text-muted">
                  {tile.hint}
                  <span className="sr-only">
                    {tile.needsAttention ? "，需要关注" : ""}
                  </span>
                </p>
              </li>
            );
          })}
        </ul>

        <div className="grid gap-3 lg:grid-cols-2">
          <Card>
            <CardContent className="p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <h3 className="text-body font-medium text-text">CI（最近 24 小时）</h3>
                <Badge variant={ci.red > 0 || ci.timeout > 0 ? "warning" : "neutral"}>
                  {ci.red + ci.timeout > 0 ? "有失败" : "无失败"}
                </Badge>
              </div>
              <div className="mt-3">
                <CiBar counts={ci} />
              </div>
              <dl className="mt-3 grid grid-cols-3 gap-2">
                {CI_24H_SEGMENTS.map((segment) => (
                  <div key={segment.key} className="min-w-0">
                    <dt className="flex items-center gap-1.5 text-caption text-text-secondary">
                      <span
                        aria-hidden="true"
                        className={`size-1.5 shrink-0 rounded-full ${segment.dot}`}
                      />
                      {segment.label}
                    </dt>
                    <dd className="mt-0.5 font-mono text-body font-semibold tabular-nums text-text">
                      {formatNumber(ci[segment.key])}
                    </dd>
                  </div>
                ))}
              </dl>
              <p className="mt-2 flex flex-wrap items-center gap-1.5 text-caption text-text-secondary">
                <span
                  aria-hidden="true"
                  className={`size-1.5 shrink-0 rounded-full ${CI_PENDING_DOT}`}
                />
                当前待定
                <span className="font-mono tabular-nums text-text">{formatNumber(ci.pending)}</span>
                <span className="text-text-muted">（处于 CI 阶段的运行，不计入 24 小时累计）</span>
              </p>
            </CardContent>
          </Card>

          <Card>
            <CardContent className="p-4">
              <h3 className="text-body font-medium text-text">今日产出</h3>
              <dl className="mt-3 grid grid-cols-2 gap-3">
                <div className="min-w-0">
                  <dt className="flex items-center gap-1.5 text-caption text-text-secondary">
                    <FileCheck2 aria-hidden="true" className="size-3.5 text-text-muted" />
                    今日上报
                  </dt>
                  <dd className="mt-0.5 font-mono text-heading font-semibold tabular-nums text-text">
                    {formatNumber(health.reports_today)}
                  </dd>
                </div>
                <div className="min-w-0">
                  <dt className="flex items-center gap-1.5 text-caption text-text-secondary">
                    <GitPullRequest aria-hidden="true" className="size-3.5 text-text-muted" />
                    今日 PR
                  </dt>
                  <dd className="mt-0.5 font-mono text-heading font-semibold tabular-nums text-text">
                    {formatNumber(health.prs_today)}
                  </dd>
                </div>
              </dl>
            </CardContent>
          </Card>
        </div>
      </div>
    </OverviewSection>
  );
}
