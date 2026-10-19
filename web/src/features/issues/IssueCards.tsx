import { ArrowRight, Clock3, Copy } from "lucide-react";
import { Link } from "react-router-dom";

import {
  Button,
  Card,
  CardContent,
  SeverityStone,
  StateBadge,
} from "../../components/ui";
import {
  formatAge,
  formatCount,
  formatDateTime,
  isoDateTime,
  issueDetailPath,
  shortFingerprint,
} from "./format";
import { CiVerdictBadge, PrLink, StuckBadge } from "./IssueBadges";
import { IssueActionButton } from "./IssueActionButton";
import type { IssueListItem } from "./types";

interface IssueCardsProps {
  issues: IssueListItem[];
  onCopyFingerprint: (issue: IssueListItem) => void;
}

/**
 * Mobile issue list (<768px, docs/09 §11.7). Cards keep severity, state,
 * title, last-seen time, the state-aware owner action and the read-only copy
 * and detail actions; the table is hidden at this breakpoint and vice versa,
 * so the accessibility tree only contains one.
 */
export function IssueCards({ issues, onCopyFingerprint }: IssueCardsProps) {
  return (
    <ul aria-label="问题单卡片列表" className="space-y-3">
      {issues.map((issue) => {
        const title = issue.title || "（无标题）";
        const detailPath = issueDetailPath(issue.fingerprint);
        return (
          <li key={issue.fingerprint}>
            <Card>
              <CardContent className="space-y-3 p-4">
                <div className="flex flex-wrap items-center gap-2">
                  <SeverityStone severity={issue.severity} />
                  <StateBadge state={issue.state} />
                  {issue.stuck ? <StuckBadge /> : null}
                  <span
                    className="ml-auto inline-flex items-center gap-1 text-caption whitespace-nowrap text-text-muted"
                    title={formatDateTime(issue.last_seen_ms)}
                  >
                    <Clock3 aria-hidden="true" className="size-3.5" />
                    <time dateTime={isoDateTime(issue.last_seen_ms)}>
                      {formatAge(issue.last_seen_ms)}
                    </time>
                  </span>
                </div>

                <div className="min-w-0">
                  <h3 className="text-heading-sm font-medium text-text">
                    <Link
                      to={detailPath}
                      className="rounded-sm transition-colors hover:text-accent"
                    >
                      {title}
                    </Link>
                  </h3>
                  <p
                    className="mt-0.5 truncate font-mono text-caption text-text-muted"
                    title={issue.fingerprint}
                  >
                    {shortFingerprint(issue.fingerprint)}
                  </p>
                </div>

                <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-caption">
                  <div className="min-w-0">
                    <dt className="text-text-muted">仓库</dt>
                    <dd
                      className="truncate font-mono text-text-secondary"
                      title={issue.repo || undefined}
                    >
                      {issue.repo || "—"}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-text-muted">出现次数</dt>
                    <dd className="font-mono tabular-nums text-text">
                      {formatCount(issue.occurrences)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-text-muted">修复尝试</dt>
                    <dd className="font-mono tabular-nums text-text">
                      {formatCount(issue.attempts ?? 0)}
                    </dd>
                  </div>
                  <div className="min-w-0">
                    <dt className="text-text-muted">PR / CI</dt>
                    <dd className="mt-0.5 flex flex-wrap items-center gap-1.5">
                      {issue.pr_url ? <PrLink url={issue.pr_url} title={title} /> : null}
                      <CiVerdictBadge verdict={issue.ci_verdict} />
                      {!issue.pr_url && !issue.ci_verdict ? (
                        <span className="text-text-muted">—</span>
                      ) : null}
                    </dd>
                  </div>
                  <div className="min-w-0">
                    <dt className="text-text-muted">归属运行器</dt>
                    <dd
                      className="truncate font-mono text-text-secondary"
                      title={issue.owner_runner_id ?? undefined}
                    >
                      {issue.owner_runner_id ?? "未归属"}
                    </dd>
                  </div>
                </dl>

                <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border pt-3">
                  <Button
                    variant="secondary"
                    size="md"
                    onClick={() => onCopyFingerprint(issue)}
                    aria-label={`复制指纹：${issue.fingerprint}`}
                  >
                    <Copy aria-hidden="true" className="size-4" />
                    复制指纹
                  </Button>
                  <IssueActionButton
                    fingerprint={issue.fingerprint}
                    state={issue.state}
                    stuck={issue.stuck}
                  />
                  <Link
                    to={detailPath}
                    aria-label={`查看详情：${title}`}
                    className="inline-flex min-h-11 items-center gap-1 rounded-card px-3 text-body-sm font-medium text-text-secondary transition-colors hover:bg-raised hover:text-accent"
                  >
                    查看详情
                    <ArrowRight aria-hidden="true" className="size-4" />
                  </Link>
                </div>
              </CardContent>
            </Card>
          </li>
        );
      })}
    </ul>
  );
}
