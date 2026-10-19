import { ArrowLeft, Clock3 } from "lucide-react";
import { type MouseEvent } from "react";
import { useLocation, useNavigate } from "react-router-dom";

import { SeverityStone, StateBadge } from "../../components/ui";
import { CiVerdictBadge, StuckBadge } from "../issues/IssueBadges";
import { IssueActionButton } from "../issues/IssueActionButton";
import type { IssueListItem } from "../issues/types";
import {
  formatAge,
  formatCount,
  formatDateTime,
  isoDateTime,
  shortFingerprint,
} from "./format";
import { CopyButton } from "./CopyButton";

interface IssueHeaderProps {
  issue: IssueListItem;
}

function MetaTime({ label, tsMs }: { label: string; tsMs: number }) {
  return (
    <div className="min-w-0">
      <dt className="text-caption text-text-muted">{label}</dt>
      <dd className="mt-0.5 text-body-sm text-text-secondary">
        <time dateTime={isoDateTime(tsMs)} title={formatDateTime(tsMs)}>
          {formatAge(tsMs)}
        </time>
      </dd>
    </div>
  );
}

/**
 * Detail header (docs/09 §10.5): severity, title, state, repo/language,
 * copyable fingerprint, occurrence count, first/last seen and the PR/CI
 * outcome. The owner action (reopen/requeue) sits next to the state and is
 * offered only for the transitions the server accepts.
 */
export function IssueHeader({ issue }: IssueHeaderProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const title = issue.title || "（无标题）";
  const fingerprint = issue.fingerprint;

  // Return to wherever the user came from (usually a filtered issue list), so
  // an "escalated 逐条复核" flow keeps its filters and page. Direct deep links
  // have no in-app history; they fall back to the plain list.
  const handleBack = (event: MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
    if (location.key !== "default") {
      navigate(-1);
    } else {
      navigate("/issues");
    }
  };

  return (
    <header className="space-y-4">
      <a
        href="/issues"
        onClick={handleBack}
        className="inline-flex min-h-11 items-center gap-1.5 rounded-sm text-body-sm text-text-secondary transition-colors hover:text-accent pointer-fine:min-h-9"
      >
        <ArrowLeft aria-hidden="true" className="size-3.5" />
        返回
      </a>

      <div className="flex flex-wrap items-start gap-x-3 gap-y-2">
        <SeverityStone severity={issue.severity} className="mt-1.5 shrink-0" />
        <h2
          id="issue-title"
          className="min-w-0 flex-1 text-title font-semibold break-words text-text"
        >
          {title}
        </h2>
        <div className="flex shrink-0 flex-wrap items-center gap-1.5">
          <StateBadge state={issue.state} />
          {issue.stuck ? <StuckBadge /> : null}
          <CiVerdictBadge verdict={issue.ci_verdict} />
          <IssueActionButton
            fingerprint={fingerprint}
            state={issue.state}
            stuck={issue.stuck}
          />
        </div>
      </div>

      <dl className="grid grid-cols-2 gap-x-4 gap-y-3 rounded-card border border-border bg-surface px-4 py-3 sm:grid-cols-3 lg:grid-cols-7">
        <div className="min-w-0">
          <dt className="text-caption text-text-muted">仓库</dt>
          <dd
            className="mt-0.5 truncate font-mono text-caption text-text-secondary"
            title={issue.repo || undefined}
          >
            {issue.repo || "—"}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-caption text-text-muted">语言</dt>
          <dd className="mt-0.5 font-mono text-caption text-text-secondary">
            {issue.language || "—"}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-caption text-text-muted">出现次数</dt>
          <dd className="mt-0.5 font-mono text-caption tabular-nums text-text">
            {formatCount(issue.occurrences)}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-caption text-text-muted">修复尝试</dt>
          <dd className="mt-0.5 font-mono text-caption tabular-nums text-text">
            {formatCount(issue.attempts ?? 0)}
          </dd>
        </div>
        <MetaTime label="首次出现" tsMs={issue.first_seen_ms} />
        <MetaTime label="最近出现" tsMs={issue.last_seen_ms} />
        <MetaTime label="状态变更" tsMs={issue.state_changed_at_ms} />
      </dl>

      <div className="flex flex-wrap items-start gap-2 rounded-card border border-border bg-surface px-4 py-3">
        <div className="min-w-0 flex-1">
          <p className="text-caption text-text-muted">指纹</p>
          <code className="mt-0.5 block font-mono text-caption break-all text-text">
            {fingerprint}
          </code>
        </div>
        <CopyButton
          value={fingerprint}
          label={`复制完整指纹：${fingerprint}`}
          toastTitle="已复制指纹"
          text="复制"
          className="shrink-0"
        />
        {issue.pr_url ? (
          <a
            href={issue.pr_url}
            target="_blank"
            rel="noreferrer"
            className="inline-flex min-h-11 shrink-0 items-center gap-1.5 rounded-card border border-border-strong bg-raised px-3 text-caption font-medium text-text transition-colors hover:border-accent hover:text-accent pointer-fine:min-h-9"
          >
            打开 PR
            <span className="sr-only">（新标签页）</span>
          </a>
        ) : (
          <span className="inline-flex min-h-9 shrink-0 items-center text-caption text-text-muted">
            尚未开 PR
          </span>
        )}
      </div>

      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-caption text-text-muted">
        <Clock3 aria-hidden="true" className="size-3.5 shrink-0" />
        <span>短指纹</span>
        <span className="font-mono" title={fingerprint}>
          {shortFingerprint(fingerprint)}
        </span>
        <span aria-hidden="true">·</span>
        <span>最近更新</span>
        <time
          dateTime={isoDateTime(issue.last_seen_ms)}
          title={formatDateTime(issue.last_seen_ms)}
        >
          {formatAge(issue.last_seen_ms)}
        </time>
      </p>
    </header>
  );
}
