import { AlertCircle, FileQuestion, RefreshCw } from "lucide-react";
import { Link } from "react-router-dom";

import { Button, Card, CardContent, EmptyState, Skeleton } from "../../components/ui";
import { describeDetailError } from "./format";

/** Initial-load skeleton; the sr-only status is the single announcement. */
export function IssueDetailSkeleton() {
  return (
    <div role="status" aria-busy="true" className="space-y-6">
      <span className="sr-only">正在加载问题单详情</span>
      <div className="space-y-3">
        <Skeleton className="h-4 w-28" />
        <Skeleton className="h-8 w-3/5" />
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-6">
          {Array.from({ length: 6 }, (_, index) => (
            <Skeleton key={index} className="h-12" />
          ))}
        </div>
        <Skeleton className="h-16" />
      </div>
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <div className="space-y-6">
          <Skeleton className="h-48" />
          <Skeleton className="h-64" />
        </div>
        <Skeleton className="h-72" />
      </div>
    </div>
  );
}

interface IssueDetailErrorProps {
  error: unknown;
  onRetry: () => void;
}

/** Non-404 load failure with a retryable, human-safe message. */
export function IssueDetailError({ error, onRetry }: IssueDetailErrorProps) {
  const { message, code } = describeDetailError(error);
  return (
    <Card className="border-danger/40">
      <CardContent className="flex flex-wrap items-start gap-3 p-4">
        {/* Only the message is a live region; the retry button remains an
            ordinary sibling outside it. */}
        <div role="alert" className="flex min-w-0 flex-1 items-start gap-3">
          <AlertCircle aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-danger" />
          <div className="min-w-0 flex-1">
            <p className="text-body font-medium text-text">无法加载问题单详情</p>
            <p className="mt-0.5 text-body-sm text-text-secondary">{message}</p>
            {code ? (
              <p className="mt-1 font-mono text-caption text-text-muted">{code}</p>
            ) : null}
          </div>
        </div>
        <Button variant="secondary" size="sm" onClick={onRetry}>
          <RefreshCw aria-hidden="true" className="size-3.5" />
          重试
        </Button>
      </CardContent>
    </Card>
  );
}

interface IssueNotFoundProps {
  fingerprint: string;
}

/** 404 for an unknown/stale fingerprint (the API answers 404 `not_found`). */
export function IssueNotFound({ fingerprint }: IssueNotFoundProps) {
  return (
    <EmptyState
      icon={FileQuestion}
      title="问题单不存在"
      description={`未找到指纹 ${fingerprint} 的问题单，可能已被清理或链接有误。`}
      action={
        <Link
          to="/issues"
          className="inline-flex min-h-11 items-center rounded-card border border-border-strong bg-raised px-3 text-caption font-medium text-text transition-colors hover:border-accent hover:text-accent pointer-fine:min-h-9"
        >
          返回问题单列表
        </Link>
      }
    />
  );
}

interface IssueDetailStaleNoticeProps {
  error: unknown;
  onRetry: () => void;
}

/** Background-refetch failure while the audit data stays on screen. */
export function IssueDetailStaleNotice({ error, onRetry }: IssueDetailStaleNoticeProps) {
  const { message, code } = describeDetailError(error);
  return (
    <div className="flex flex-wrap items-start justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text">
      <div role="status" className="min-w-0 flex-1">
        <p className="font-medium">详情更新失败，正在显示上次数据。</p>
        <p className="mt-0.5 text-text-secondary">{message}</p>
        {code ? <p className="mt-1 font-mono text-caption text-text-muted">{code}</p> : null}
      </div>
      <Button variant="ghost" size="sm" onClick={onRetry}>
        <RefreshCw aria-hidden="true" className="size-3.5" />
        重试
      </Button>
    </div>
  );
}
