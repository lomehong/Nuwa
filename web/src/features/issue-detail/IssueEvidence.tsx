import { Badge, Card, CardContent, CardHeader, CardTitle } from "../../components/ui";
import { CiVerdictBadge, PrLink } from "../issues/IssueBadges";
import { CopyButton } from "./CopyButton";
import {
  formatAge,
  formatDateTime,
  isoDateTime,
  runDurationMs,
  runOutcomeLabel,
  runOutcomeVariant,
  runPhaseLabel,
  shortRunId,
} from "./format";
import { IssueSection } from "./IssueSection";
import type { RunDto } from "./types";

interface IssueEvidenceProps {
  /** The run the evidence describes (selected or newest); `undefined` means
   * the runner has not claimed the issue. */
  run: RunDto | undefined;
  /** 1-based attempt index of `run` within the issue's recent runs; 0 hides it. */
  attempt?: number;
}

function VerificationBlock({ run }: { run: RunDto }) {
  const status =
    run.verification_ok === null
      ? "未记录结果"
      : run.verification_ok
        ? "通过"
        : "失败";
  const detail = run.verification_detail?.trim();

  return (
    <div className="min-w-0">
      <h5 className="text-caption font-medium text-text-muted">验证</h5>
      <div className="mt-1 flex flex-wrap items-center gap-1.5">
        <Badge
          variant={
            run.verification_ok === null ? "neutral" : run.verification_ok ? "success" : "danger"
          }
        >
          {status}
        </Badge>
        {run.error ? (
          <span className="text-caption break-words text-danger">{run.error}</span>
        ) : null}
      </div>
      {detail ? (
        <details className="group mt-2 rounded-card border border-border bg-surface">
          <summary className="flex min-h-11 cursor-pointer list-none items-center gap-2 px-3 py-1.5 text-caption font-medium text-text-secondary transition-colors hover:text-text pointer-fine:min-h-9 [&::-webkit-details-marker]:hidden">
            <span
              aria-hidden="true"
              className="font-mono text-text-muted transition-transform duration-150 group-open:rotate-90"
            >
              ▸
            </span>
            验证输出
            <span className="font-normal text-text-muted">（等宽、可复制）</span>
          </summary>
          <div className="border-t border-border p-2">
            <pre className="max-h-80 overflow-auto rounded-sm bg-raised/60 p-2 font-mono text-caption break-words whitespace-pre-wrap text-text-secondary">
              {detail}
            </pre>
            <div className="mt-1 flex justify-end">
              <CopyButton
                value={detail}
                label="复制验证输出"
                toastTitle="已复制验证输出"
                text="复制"
              />
            </div>
          </div>
        </details>
      ) : (
        <p className="mt-1 text-caption text-text-muted">未记录验证输出。</p>
      )}
    </div>
  );
}

function BranchCommitBlock({ run }: { run: RunDto }) {
  return (
    <div className="min-w-0">
      <h5 className="text-caption font-medium text-text-muted">分支与提交</h5>
      <dl className="mt-1 space-y-1.5">
        <div className="flex flex-wrap items-center gap-1">
          <dt className="text-caption text-text-muted">分支</dt>
          <dd className="min-w-0 font-mono text-caption break-all text-text-secondary">
            {run.branch || "—"}
          </dd>
          {run.branch ? (
            <CopyButton
              value={run.branch}
              label={`复制分支名：${run.branch}`}
              toastTitle="已复制分支名"
            />
          ) : null}
        </div>
        <div className="flex flex-wrap items-center gap-1">
          <dt className="text-caption text-text-muted">提交</dt>
          <dd className="min-w-0 font-mono text-caption break-all text-text-secondary">
            {run.commit_sha || "—"}
          </dd>
          {run.commit_sha ? (
            <CopyButton
              value={run.commit_sha}
              label={`复制提交 SHA：${run.commit_sha}`}
              toastTitle="已复制提交 SHA"
            />
          ) : null}
        </div>
      </dl>
    </div>
  );
}

/**
 * Latest-run evidence panel (docs/09 §10.5): root cause, change stat,
 * verification output (collapsed, monospace, copyable), CI verdict and the
 * branch/commit/PR triple. Tabs from the design sketch are rendered as a
 * single audit view because all four fields are needed together to judge a
 * repair; each block keeps its own heading for navigation.
 */
export function IssueEvidence({ run, attempt = 0 }: IssueEvidenceProps) {
  return (
    <IssueSection
      id="issue-evidence"
      title="证据"
      description="所选运行留下的根因、改动、验证与 CI 结论。"
      aside={
        run ? (
          <Badge variant={runOutcomeVariant(run.outcome)}>
            {runOutcomeLabel(run.outcome)}
          </Badge>
        ) : null
      }
    >
      <Card>
        {run ? (
          <>
            <CardHeader className="py-3">
              <div className="min-w-0">
                <CardTitle as="h4">
                  {attempt > 0 ? `第 ${attempt} 次运行的证据` : "运行证据"}
                </CardTitle>
                <p className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-caption text-text-muted">
                  <span className="font-mono" title={run.id}>
                    {shortRunId(run.id)}
                  </span>
                  <span aria-hidden="true">·</span>
                  <span>{runPhaseLabel(run.phase)}</span>
                  <span aria-hidden="true">·</span>
                  <time
                    dateTime={isoDateTime(run.started_at_ms)}
                    title={formatDateTime(run.started_at_ms)}
                  >
                    {formatAge(run.started_at_ms)}
                  </time>
                  <span aria-hidden="true">·</span>
                  <span>{formatDurationLabel(run)}</span>
                </p>
              </div>
            </CardHeader>
            <CardContent className="grid gap-5 p-4 md:grid-cols-2">
              <div className="min-w-0 md:col-span-2">
                <h5 className="text-caption font-medium text-text-muted">根因</h5>
                <p className="mt-1 text-body-sm break-words whitespace-pre-wrap text-text">
                  {run.root_cause?.trim() || "未记录根因。"}
                </p>
              </div>

              <div className="min-w-0">
                <h5 className="text-caption font-medium text-text-muted">改动</h5>
                {run.change_stat?.trim() ? (
                  <pre className="mt-1 max-h-48 overflow-auto rounded-card border border-border bg-raised/50 p-2 font-mono text-caption break-words whitespace-pre-wrap text-text-secondary">
                    {run.change_stat}
                  </pre>
                ) : (
                  <p className="mt-1 text-caption text-text-muted">未记录改动统计。</p>
                )}
              </div>

              <VerificationBlock run={run} />

              <div className="min-w-0">
                <h5 className="text-caption font-medium text-text-muted">CI</h5>
                <div className="mt-1 flex flex-wrap items-center gap-1.5">
                  <CiVerdictBadge verdict={run.ci_verdict} />
                  {!run.ci_verdict ? (
                    <span className="text-caption text-text-muted">未记录 CI 结论。</span>
                  ) : null}
                  {run.pr_url ? <PrLink url={run.pr_url} title={run.id} /> : null}
                  {!run.pr_url ? (
                    <span className="text-caption text-text-muted">本次运行未开 PR。</span>
                  ) : null}
                </div>
                {run.pr_opened_at_ms !== null ? (
                  <p className="mt-1 text-caption text-text-muted">
                    PR 打开于{" "}
                    <time
                      dateTime={isoDateTime(run.pr_opened_at_ms)}
                      title={formatDateTime(run.pr_opened_at_ms)}
                    >
                      {formatAge(run.pr_opened_at_ms)}
                    </time>
                  </p>
                ) : null}
              </div>

              <BranchCommitBlock run={run} />
            </CardContent>
          </>
        ) : (
          <CardContent className="p-4">
            <p className="text-body font-medium text-text">等待 runner 领取</p>
            <p className="mt-0.5 text-body-sm text-text-secondary">
              该问题单还没有运行记录，领取后会在这里出现根因、改动与验证证据。
            </p>
          </CardContent>
        )}
      </Card>
    </IssueSection>
  );
}

function formatDurationLabel(run: RunDto): string {
  if (run.finished_at_ms === null) {
    return "进行中";
  }
  const ms = runDurationMs(run);
  const seconds = Math.round(ms / 1_000);
  if (seconds < 60) {
    return `${seconds} 秒`;
  }
  const minutes = Math.floor(seconds / 60);
  return `${minutes} 分 ${seconds % 60} 秒`;
}
