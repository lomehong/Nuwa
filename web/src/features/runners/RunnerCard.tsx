import { Clock3, Cpu } from "lucide-react";
import { Link } from "react-router-dom";

import { Badge, Card, CardContent } from "../../components/ui";
import { CopyButton } from "../issue-detail/CopyButton";
import { issueDetailPath } from "../issues/format";
import { RUN_PHASES, runPhaseIndex } from "../overview/ActiveRuns";
import {
  formatAge,
  formatDateTime,
  isoDateTime,
  shortFingerprint,
} from "../overview/format";
import {
  runnerIsOffline,
  runnerQuotaPercent,
  runnerStatusLabel,
  runnerStatusVariant,
} from "./format";
import { RestartRunnerButton } from "./RestartRunnerButton";
import { ProcessConfiguration } from "../settings/ProcessConfiguration";
import type { RunnerDto } from "./types";

/** Chinese phase label for a current run; unknown values stay verbatim. */
function phaseLabel(phase: string): string {
  const index = runPhaseIndex(phase);
  return index >= 0 ? RUN_PHASES[index].label : phase;
}

interface RunnerCardProps {
  runner: RunnerDto;
  /** Staleness is computed against the page's render clock, not the API flag. */
  nowMs: number;
}

/**
 * One runner in the fleet grid (docs/09 §10.6). Identity and actions copy to
 * the clipboard, link to the issue being worked on, or schedule a restart of
 * the runner's systemd unit behind an impact confirmation.
 */
export function RunnerCard({ runner, nowMs }: RunnerCardProps) {
  const offline = runnerIsOffline(runner, nowMs);
  const quotaPercent = runnerQuotaPercent(runner);
  const quotaLimit =
    runner.daily_pr_limit !== null && runner.daily_pr_limit > 0
      ? runner.daily_pr_limit
      : null;
  const currentPath = runner.current_fingerprint
    ? issueDetailPath(runner.current_fingerprint)
    : null;

  return (
    <Card data-testid={`runner-card-${runner.id}`}>
      <CardContent className="space-y-3 p-5">
        <div className="flex items-start gap-3">
          <span
            aria-hidden="true"
            className="flex size-9 shrink-0 items-center justify-center rounded-full bg-raised text-text-muted"
          >
            <Cpu className="size-4" strokeWidth={1.75} />
          </span>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-1.5">
              <span className="font-mono text-body-sm font-semibold text-text">
                {runner.id}
              </span>
              <CopyButton value={runner.id} label={`复制运行器 ID：${runner.id}`} />
              <RestartRunnerButton runner={runner} />
              <Badge variant={runnerStatusVariant(runner, nowMs)} className="ml-auto">
                {runnerStatusLabel(runner, nowMs)}
              </Badge>
            </div>
            <p
              className="mt-0.5 truncate font-mono text-caption text-text-secondary"
              title={`${runner.repo} · ${runner.host}`}
            >
              {runner.repo || "—"} · {runner.host || "—"}
            </p>
            <p
              className="mt-1 inline-flex items-center gap-1 text-caption text-text-muted"
              title={formatDateTime(runner.heartbeat_at_ms)}
            >
              <Clock3 aria-hidden="true" className="size-3.5" />
              <span>
                最近心跳{" "}
                <time dateTime={isoDateTime(runner.heartbeat_at_ms)}>
                  {formatAge(runner.heartbeat_at_ms, nowMs)}
                </time>
              </span>
            </p>
          </div>
        </div>

        <div className="rounded-card bg-raised px-3 py-2">
          <p className="text-caption text-text-muted">当前任务</p>
          {currentPath ? (
            <p className="mt-0.5 flex flex-wrap items-center gap-1.5 text-body-sm">
              <Link
                to={currentPath}
                title={runner.current_fingerprint ?? undefined}
                aria-label={`查看当前单：${runner.current_fingerprint}`}
                className="inline-flex min-h-11 items-center rounded-sm font-mono text-text-secondary transition-colors hover:text-accent pointer-fine:min-h-9"
              >
                {shortFingerprint(runner.current_fingerprint ?? "")}
              </Link>
              {runner.phase ? <Badge variant="info">{phaseLabel(runner.phase)}</Badge> : null}
              {offline ? <Badge variant="danger">心跳过期</Badge> : null}
            </p>
          ) : (
            <p className="mt-0.5 text-body-sm text-text-secondary">队列为空</p>
          )}
        </div>

        <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-caption">
          <div className="min-w-0">
            <dt className="text-text-muted">心跳 Provider / 模型（非版本证据）</dt>
            <dd
              className="truncate font-mono text-text-secondary"
              title={`${runner.provider ?? "—"} / ${runner.model ?? "—"}`}
            >
              {runner.provider ?? "—"} / {runner.model ?? "—"}
            </dd>
          </div>
          <div className="min-w-0">
            <dt className="text-text-muted">仓库路径</dt>
            <dd
              className="truncate font-mono text-text-secondary"
              title={runner.repo_path ?? undefined}
            >
              {runner.repo_path ?? "—"}
            </dd>
          </div>
          <div className="col-span-2 min-w-0">
            <dt className="text-text-muted">服务单元</dt>
            <dd className="flex min-w-0 items-center gap-1">
              <span
                className="truncate font-mono text-text-secondary"
                title={runner.service_unit ?? undefined}
              >
                {runner.service_unit ?? "—"}
              </span>
              {runner.service_unit ? (
                <CopyButton
                  value={runner.service_unit}
                  label={`复制服务单元：${runner.service_unit}`}
                />
              ) : null}
            </dd>
          </div>
        </dl>

        <div className="space-y-1">
          <div className="flex items-center justify-between gap-2 text-caption">
            <span className="text-text-muted">UTC 准入槽位 / 本进程上报值</span>
            <span className="inline-flex items-center gap-1 font-mono tabular-nums text-text-secondary">
              <span>
                {`${runner.pr_slots_today}/${quotaLimit ?? "—"}`}
              </span>
              {quotaPercent !== null ? (
                <span className="text-text-muted">{quotaPercent}%</span>
              ) : null}
            </span>
          </div>
          {quotaLimit !== null ? (
            <progress
              aria-label="UTC 准入槽位 / 本进程上报值"
              value={runner.pr_slots_today}
              max={quotaLimit}
              className="h-1.5 w-full accent-accent"
            />
          ) : null}
          {quotaLimit === null ? <p className="text-caption text-text-muted">未上报准入预算</p> : null}
          <p className="text-caption text-text-muted">
            UTC 今日确认 PR（创建 / 本地观测记录）：{runner.prs_today}。
            槽位含在途、结果不明及已消耗；重试保留原准入日。
            上报值是观测，不是中心统一硬上限；策略由创建进程解析，各进程可能不同。
          </p>
        </div>

        <ProcessConfiguration process={runner.configuration} />

        {runner.last_error ? (
          <div className="rounded-card border border-danger/40 bg-danger/10 px-3 py-2">
            <p className="text-caption text-text-muted">最近错误</p>
            <p className="mt-0.5 break-words font-mono text-caption text-text">
              {runner.last_error}
            </p>
          </div>
        ) : null}
      </CardContent>
    </Card>
  );
}
