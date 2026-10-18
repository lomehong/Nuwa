import { Badge, Card, CardContent, ISSUE_STATE_VALUES, SEVERITY_VALUES, SeverityStone, StateBadge } from "../../components/ui";
import type { IssueState, Severity } from "../../components/ui";
import { Link } from "react-router-dom";
import { issueDetailPath } from "../issues/format";
import type { IssueListItem } from "../issues/types";
import { CopyButton } from "./CopyButton";
import {
  formatAge,
  formatCount,
  formatDateTime,
  formatDuration,
  formatTokens,
  runDurationMs,
  runOutcomeLabel,
  runOutcomeVariant,
  runPhaseLabel,
  runTokenTotal,
  shortFingerprint,
} from "./format";
import { IssueSection } from "./IssueSection";
import type { RunDto, SimilarItem } from "./types";

function asSeverity(value: string | undefined): Severity | null {
  if (!value) {
    return null;
  }
  return SEVERITY_VALUES.includes(value as Severity) ? (value as Severity) : null;
}

function asIssueState(value: string): IssueState | null {
  return ISSUE_STATE_VALUES.includes(value as IssueState) ? (value as IssueState) : null;
}

interface SimilarItemRowProps {
  item: SimilarItem;
}

function SimilarItemRow({ item }: SimilarItemRowProps) {
  const severity = asSeverity(item.severity);
  const state = asIssueState(item.state);

  return (
    <li className="min-w-0 py-2 first:pt-0 last:pb-0">
      <div className="flex flex-wrap items-center gap-1.5">
        {severity ? <SeverityStone severity={severity} /> : null}
        {state ? <StateBadge state={state} /> : <Badge>{item.state || "未知状态"}</Badge>}
        {item.occurrences !== undefined ? (
          <span className="text-caption text-text-muted">
            {formatCount(item.occurrences)} 次
          </span>
        ) : null}
      </div>
      <p className="mt-1 min-w-0 text-body-sm">
        <Link
          to={issueDetailPath(item.fingerprint)}
          className="rounded-sm font-medium break-words text-text transition-colors hover:text-accent"
          title={item.fingerprint}
        >
          {item.title || shortFingerprint(item.fingerprint)}
        </Link>
      </p>
      <p className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-caption text-text-muted">
        {item.repo ? (
          <span className="min-w-0 truncate font-mono" title={item.repo}>
            {item.repo}
          </span>
        ) : (
          <span className="font-mono" title={item.fingerprint}>
            {shortFingerprint(item.fingerprint)}
          </span>
        )}
        <span aria-hidden="true">·</span>
        <time
          dateTime={new Date(item.last_seen_ms).toISOString()}
          title={formatDateTime(item.last_seen_ms)}
        >
          {formatAge(item.last_seen_ms)}
        </time>
      </p>
    </li>
  );
}

interface IssueSidebarProps {
  issue: IssueListItem;
  latestRun: RunDto | undefined;
  similarCount: number;
  similarItems: SimilarItem[];
}

/**
 * Detail sidebar (docs/09 §10.5): raw identity/ownership metadata, the latest
 * run summary and same-title neighbours. Under 1024px this column stacks under
 * the main timeline/evidence column.
 */
export function IssueSidebar({ issue, latestRun, similarCount, similarItems }: IssueSidebarProps) {
  return (
    <div className="space-y-6">
      <IssueSection id="issue-meta" title="元数据">
        <Card>
          <CardContent className="space-y-3 p-4">
            <div>
              <p className="text-caption text-text-muted">完整指纹</p>
              <div className="flex flex-wrap items-start gap-1">
                <code className="min-w-0 font-mono text-caption break-all text-text">
                  {issue.fingerprint}
                </code>
                <CopyButton
                  value={issue.fingerprint}
                  label={`复制完整指纹：${issue.fingerprint}`}
                  toastTitle="已复制指纹"
                />
              </div>
            </div>
            <dl className="space-y-2 text-caption">
              <div>
                <dt className="text-text-muted">归属运行器</dt>
                <dd className="mt-0.5 font-mono break-all text-text-secondary">
                  {issue.owner_runner_id || "未归属"}
                </dd>
              </div>
              <div>
                <dt className="text-text-muted">状态变更于</dt>
                <dd className="mt-0.5 text-text-secondary">
                  <time
                    dateTime={new Date(issue.state_changed_at_ms).toISOString()}
                    title={formatDateTime(issue.state_changed_at_ms)}
                  >
                    {formatAge(issue.state_changed_at_ms)}
                  </time>
                </dd>
              </div>
              <div>
                <dt className="text-text-muted">仓库 / 语言</dt>
                <dd className="mt-0.5 break-words text-text-secondary">
                  <span className="font-mono">{issue.repo || "—"}</span>
                  <span className="mx-1" aria-hidden="true">
                    ·
                  </span>
                  <span className="font-mono">{issue.language || "—"}</span>
                </dd>
              </div>
            </dl>
          </CardContent>
        </Card>
      </IssueSection>

      <IssueSection id="issue-latest-run" title="最近运行">
        <Card>
          <CardContent className="space-y-2 p-4">
            {latestRun ? (
              <>
                <div className="flex flex-wrap items-center gap-1.5">
                  <Badge>{runPhaseLabel(latestRun.phase)}</Badge>
                  <Badge variant={runOutcomeVariant(latestRun.outcome)}>
                    {runOutcomeLabel(latestRun.outcome)}
                  </Badge>
                </div>
                <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-caption">
                  <div>
                    <dt className="text-text-muted">开始</dt>
                    <dd className="mt-0.5 text-text-secondary">
                      <time
                        dateTime={new Date(latestRun.started_at_ms).toISOString()}
                        title={formatDateTime(latestRun.started_at_ms)}
                      >
                        {formatAge(latestRun.started_at_ms)}
                      </time>
                    </dd>
                  </div>
                  <div>
                    <dt className="text-text-muted">耗时</dt>
                    <dd className="mt-0.5 font-mono tabular-nums text-text-secondary">
                      {latestRun.finished_at_ms === null
                        ? "进行中"
                        : formatDuration(runDurationMs(latestRun))}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-text-muted">Turns</dt>
                    <dd className="mt-0.5 font-mono tabular-nums text-text-secondary">
                      {latestRun.turns}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-text-muted">Tokens</dt>
                    <dd
                      className="mt-0.5 font-mono tabular-nums text-text-secondary"
                      title={`未缓存 ${latestRun.tokens_uncached} · 缓存 ${latestRun.tokens_cached}`}
                    >
                      {formatTokens(runTokenTotal(latestRun))}
                    </dd>
                  </div>
                  <div className="col-span-2">
                    <dt className="text-text-muted">Provider / 模型</dt>
                    <dd className="mt-0.5 truncate font-mono text-text-secondary">
                      {[latestRun.provider, latestRun.model].filter(Boolean).join(" / ") || "—"}
                    </dd>
                  </div>
                  <div className="col-span-2">
                    <dt className="text-text-muted">运行器</dt>
                    <dd className="mt-0.5 font-mono break-all text-text-secondary">
                      {issue.owner_runner_id || "未归属"}
                    </dd>
                  </div>
                </dl>
              </>
            ) : (
              <>
                <p className="text-body font-medium text-text">等待 runner 领取</p>
                <p className="text-body-sm text-text-secondary">
                  尚无运行记录，领取后这里会显示阶段、耗时与 token 用量。
                </p>
              </>
            )}
          </CardContent>
        </Card>
      </IssueSection>

      <IssueSection
        id="issue-similar"
        title="相似问题"
        description="与当前问题单标题完全相同的其他指纹。"
        aside={<Badge>{similarCount} 个</Badge>}
      >
        <Card>
          <CardContent className="p-4">
            {similarItems.length === 0 ? (
              <p className="text-body-sm text-text-secondary">没有同名问题单。</p>
            ) : (
              <>
                <p className="mb-2 text-caption text-text-muted">
                  共 {similarCount} 个同名问题单，排除当前指纹。
                </p>
                <ul className="divide-y divide-border">
                  {similarItems.map((item) => (
                    <SimilarItemRow key={item.fingerprint} item={item} />
                  ))}
                </ul>
                {similarCount > similarItems.length ? (
                  <p className="mt-2 text-caption text-text-muted">
                    仅显示最近 {similarItems.length} 个。
                  </p>
                ) : null}
              </>
            )}
          </CardContent>
        </Card>
      </IssueSection>
    </div>
  );
}
