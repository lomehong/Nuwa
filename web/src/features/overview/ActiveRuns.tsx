import {
  ArrowRight,
  Circle,
  CircleCheck,
  CircleDot,
  Inbox,
  ListChecks,
} from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";

import {
  Badge,
  Card,
  CardContent,
  SeverityStone,
} from "../../components/ui";
import { issueDetailPath } from "./AttentionQueue";
import { formatDuration, formatTokens, shortFingerprint } from "./format";
import { OverviewSection } from "./PanelChrome";
import type { OverviewActiveRun, OverviewQueue } from "./types";

/** Claim → repair → verify → deliver → CI (docs/09 §10.3.2). */
export const RUN_PHASES = [
  { id: "claim", label: "领取" },
  { id: "repair", label: "修复" },
  { id: "verify", label: "验证" },
  { id: "deliver", label: "交付" },
  { id: "ci", label: "CI" },
] as const;

/** Index of `phase` in the stepper, or -1 for an unknown server value. */
export function runPhaseIndex(phase: string): number {
  return RUN_PHASES.findIndex((step) => step.id === phase);
}

/** Re-render elapsed times without waiting for the next 10s overview poll. */
function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(timer);
  }, [intervalMs]);
  return now;
}

function phaseState(index: number, currentIndex: number): "done" | "current" | "pending" {
  if (currentIndex < 0 || index > currentIndex) {
    return "pending";
  }
  return index === currentIndex ? "current" : "done";
}

function PhaseStepper({ phase }: { phase: string }) {
  const currentIndex = runPhaseIndex(phase);
  const currentLabel = currentIndex >= 0 ? RUN_PHASES[currentIndex].label : phase;

  return (
    <div className="min-w-0">
      <p className="sr-only">
        {currentIndex >= 0
          ? `阶段：${currentLabel}（当前）`
          : `阶段：${currentLabel}（未识别阶段）`}
      </p>
      <ol aria-label="运行阶段" className="flex items-center gap-1 overflow-x-auto py-1">
        {RUN_PHASES.map((step, index) => {
          const state = phaseState(index, currentIndex);
          return (
            <li
              key={step.id}
              aria-current={state === "current" ? "step" : undefined}
              className="flex shrink-0 items-center gap-1"
            >
              {index > 0 ? (
                <span
                  aria-hidden="true"
                  className={state === "pending" ? "h-px w-2 bg-border" : "h-px w-2 bg-success/60"}
                />
              ) : null}
              <span
                className={
                  state === "current"
                    ? "inline-flex items-center gap-1 text-caption font-medium text-text"
                    : "inline-flex items-center gap-1 text-caption text-text-muted"
                }
              >
                {state === "done" ? (
                  <CircleCheck aria-hidden="true" className="size-3.5 text-success" />
                ) : state === "current" ? (
                  <CircleDot aria-hidden="true" className="size-4 text-accent" />
                ) : (
                  <Circle aria-hidden="true" className="size-3.5 text-text-muted" />
                )}
                {step.label}
                <span className="sr-only">
                  {state === "done" ? "（已完成）" : state === "current" ? "（当前）" : "（未开始）"}
                </span>
              </span>
            </li>
          );
        })}
      </ol>
    </div>
  );
}

interface ActiveRunsProps {
  runs: OverviewActiveRun[];
  queue: OverviewQueue;
}

/**
 * "正在发生" (docs/09 §10.3.2). Active runs show the five-phase stepper,
 * elapsed time and turn/token usage. When the pool is idle the card becomes a
 * claim-queue preview so the page still answers "what happens next?".
 */
export function ActiveRuns({ runs, queue }: ActiveRunsProps) {
  const now = useNow();

  return (
    <OverviewSection
      id="overview-active-runs"
      title="正在发生"
      description="运行器当前修复的问题单与所处阶段。"
      aside={
        <>
          <Badge variant="info">
            运行中 <span className="font-mono tabular-nums">{runs.length}</span>
          </Badge>
          {queue.ready_count > 0 ? (
            <Badge>
              待领取 <span className="font-mono tabular-nums">{queue.ready_count}</span>
            </Badge>
          ) : null}
        </>
      }
    >
      {runs.length === 0 ? (
        <Card>
          <CardContent className="space-y-3 p-4">
            <div className="flex items-center gap-2">
              <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-raised text-text-muted">
                <Inbox aria-hidden="true" className="size-4" />
              </span>
              <p className="text-body font-medium text-text">当前没有活动运行</p>
            </div>
            <p className="text-body-sm text-text-secondary">
              {queue.ready_count > 0
                ? `队列中有 ${queue.ready_count} 条待领取，运行器下一次轮询会领取队首问题。`
                : "队列为空，等待新的问题上报。"}
            </p>
            {queue.next.length > 0 ? (
              <div>
                <p className="flex items-center gap-1.5 text-caption text-text-muted">
                  <ListChecks aria-hidden="true" className="size-3.5" />
                  队首（最多 3 条）
                </p>
                <ol className="mt-2 space-y-1.5">
                  {queue.next.slice(0, 3).map((item) => (
                    <li key={item.fingerprint} className="flex min-w-0 items-center gap-2">
                      <SeverityStone severity={item.severity} />
                      <Link
                        to={issueDetailPath(item.fingerprint)}
                        className="min-w-0 flex-1 truncate rounded-sm text-body-sm text-text transition-colors hover:text-accent"
                        title={item.fingerprint}
                      >
                        {item.title || shortFingerprint(item.fingerprint)}
                      </Link>
                      <span className="shrink-0 text-caption text-text-muted">
                        {item.occurrences} 次
                      </span>
                    </li>
                  ))}
                </ol>
              </div>
            ) : null}
          </CardContent>
        </Card>
      ) : (
        <ul className="grid gap-3 lg:grid-cols-2">
          {runs.map((run) => (
            <li key={run.id} className="min-w-0">
              <Card className="h-full">
                <CardContent className="flex h-full flex-col gap-3 p-4">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <h3 className="text-heading-sm font-medium text-text">
                        <Link
                          to={issueDetailPath(run.fingerprint)}
                          className="rounded-sm transition-colors hover:text-accent"
                        >
                          {run.title || shortFingerprint(run.fingerprint)}
                        </Link>
                      </h3>
                      <p
                        className="mt-1 truncate font-mono text-caption text-text-muted"
                        title={run.fingerprint}
                      >
                        {shortFingerprint(run.fingerprint)} · {run.repo || "—"}
                      </p>
                    </div>
                    <Badge variant="info">运行中</Badge>
                  </div>

                  <PhaseStepper phase={run.phase} />

                  <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-caption sm:grid-cols-4">
                    <div>
                      <dt className="text-text-muted">用时</dt>
                      <dd className="font-mono tabular-nums text-text-secondary">
                        {formatDuration(now - run.started_at_ms)}
                      </dd>
                    </div>
                    <div>
                      <dt className="text-text-muted">turns</dt>
                      <dd className="font-mono tabular-nums text-text-secondary">{run.turns}</dd>
                    </div>
                    <div>
                      <dt className="text-text-muted">tokens</dt>
                      <dd className="font-mono tabular-nums text-text-secondary">
                        {formatTokens(run.tokens)}
                      </dd>
                    </div>
                    <div className="min-w-0">
                      <dt className="text-text-muted">运行器</dt>
                      <dd className="truncate font-mono text-text-secondary" title={run.runner_id}>
                        {run.runner_id || "—"}
                      </dd>
                    </div>
                  </dl>

                  <div className="mt-auto flex justify-end border-t border-border pt-2 text-caption">
                    <Link
                      to={issueDetailPath(run.fingerprint)}
                      aria-label={`查看运行详情：${run.title || run.fingerprint}`}
                      className="inline-flex items-center gap-1 rounded-sm font-medium text-text-secondary transition-colors hover:text-accent"
                    >
                      查看详情
                      <ArrowRight aria-hidden="true" className="size-3.5" />
                    </Link>
                  </div>
                </CardContent>
              </Card>
            </li>
          ))}
        </ul>
      )}
    </OverviewSection>
  );
}
