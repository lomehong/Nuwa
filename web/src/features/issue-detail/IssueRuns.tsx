import { Badge, Button, Card, CardContent } from "../../components/ui";
import type { BadgeVariant } from "../../components/ui";
import { PrLink } from "../issues/IssueBadges";
import { CopyButton } from "./CopyButton";
import {
  formatAge,
  formatDateTime,
  formatDuration,
  formatTokens,
  isoDateTime,
  runDurationMs,
  runOutcomeLabel,
  runOutcomeVariant,
  runPhaseLabel,
  runPhaseVariant,
  runTokenTotal,
  shortRunId,
} from "./format";
import { IssueSection } from "./IssueSection";
import type { RunDto } from "./types";

interface IssueRunsProps {
  runs: RunDto[];
  /** `run_id` currently selected for the transcript, if any. */
  selectedRunId: string | null;
  onSelectRun: (runId: string) => void;
}

function RunRow({
  run,
  selected,
  onSelect,
}: {
  run: RunDto;
  selected: boolean;
  onSelect: () => void;
}) {
  const duration = run.finished_at_ms === null ? "进行中" : formatDuration(runDurationMs(run));
  const outcomeVariant: BadgeVariant = runOutcomeVariant(run.outcome);
  const provider = [run.provider, run.model].filter(Boolean).join(" / ") || "—";

  return (
    <li className="min-w-0 px-4 py-3">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge variant={runPhaseVariant(run.phase)}>{runPhaseLabel(run.phase)}</Badge>
            <Badge variant={outcomeVariant}>{runOutcomeLabel(run.outcome)}</Badge>
            {selected ? <Badge variant="accent">当前会话</Badge> : null}
          </div>
          <p className="mt-1 font-mono text-caption break-all text-text-muted" title={run.id}>
            {shortRunId(run.id)}
          </p>
        </div>
        <Button
          variant={selected ? "secondary" : "ghost"}
          size="sm"
          aria-pressed={selected}
          aria-label={
            selected
              ? `正在查看会话：${run.id}`
              : `查看会话：${run.id}`
          }
          onClick={onSelect}
        >
          {selected ? "正在查看" : "查看会话"}
        </Button>
      </div>

      <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-2 text-caption sm:grid-cols-4">
        <div className="min-w-0">
          <dt className="text-text-muted">开始</dt>
          <dd className="mt-0.5 text-text-secondary">
            <time
              dateTime={isoDateTime(run.started_at_ms)}
              title={formatDateTime(run.started_at_ms)}
            >
              {formatAge(run.started_at_ms)}
            </time>
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-text-muted">耗时</dt>
          <dd className="mt-0.5 font-mono tabular-nums text-text-secondary">{duration}</dd>
        </div>
        <div className="min-w-0">
          <dt className="text-text-muted">Turns</dt>
          <dd className="mt-0.5 font-mono tabular-nums text-text-secondary">{run.turns}</dd>
        </div>
        <div className="min-w-0">
          <dt className="text-text-muted">Tokens</dt>
          <dd
            className="mt-0.5 font-mono tabular-nums text-text-secondary"
            title={`未缓存 ${run.tokens_uncached} · 缓存 ${run.tokens_cached}`}
          >
            {formatTokens(runTokenTotal(run))}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-text-muted">Provider / 模型</dt>
          <dd className="mt-0.5 truncate font-mono text-text-secondary" title={provider}>
            {provider}
          </dd>
        </div>
        <div className="col-span-2 min-w-0">
          <dt className="text-text-muted">分支 / 提交</dt>
          <dd className="mt-0.5 space-y-0.5">
            <div className="flex flex-wrap items-center gap-1">
              <span className="min-w-0 font-mono break-all text-text-secondary">
                {run.branch || "—"}
              </span>
              {run.branch ? (
                <CopyButton
                  value={run.branch}
                  label={`复制分支名：${run.branch}`}
                  toastTitle="已复制分支名"
                />
              ) : null}
            </div>
            <div className="flex flex-wrap items-center gap-1">
              <span className="min-w-0 font-mono break-all text-text-secondary">
                {run.commit_sha || "—"}
              </span>
              {run.commit_sha ? (
                <CopyButton
                  value={run.commit_sha}
                  label={`复制提交 SHA：${run.commit_sha}`}
                  toastTitle="已复制提交 SHA"
                />
              ) : null}
            </div>
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-text-muted">PR</dt>
          <dd className="mt-0.5">
            {run.pr_url ? (
              <PrLink url={run.pr_url} title={run.id} />
            ) : (
              <span className="text-text-muted">—</span>
            )}
          </dd>
        </div>
      </dl>

      {run.error ? (
        <p className="mt-2 rounded-card border border-danger/40 bg-danger/10 px-2 py-1 text-caption break-words text-text">
          错误：{run.error}
        </p>
      ) : null}
    </li>
  );
}

/**
 * "运行记录" (docs/09 §10.5): the newest 10 runs with phase/outcome, timing,
 * turns, tokens, provider, branch/commit and PR. "查看会话" selects the run
 * for the transcript viewer via the page's `run_id` URL parameter.
 */
export function IssueRuns({ runs, selectedRunId, onSelectRun }: IssueRunsProps) {
  return (
    <IssueSection
      id="issue-runs"
      title="运行记录"
      description="最近 10 次修复运行；选择一次运行以查看对应会话。"
      aside={<Badge>{runs.length} 条</Badge>}
    >
      <Card>
        <CardContent className="p-0">
          {runs.length === 0 ? (
            <p className="px-4 py-6 text-body-sm text-text-secondary">
              暂无运行记录，等待 runner 领取。
            </p>
          ) : (
            <ol className="divide-y divide-border" aria-label="运行记录列表">
              {runs.map((run) => (
                <RunRow
                  key={run.id}
                  run={run}
                  selected={run.id === selectedRunId}
                  onSelect={() => onSelectRun(run.id)}
                />
              ))}
            </ol>
          )}
        </CardContent>
      </Card>
    </IssueSection>
  );
}
