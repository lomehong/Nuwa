import { AlertCircle, FileQuestion, ListTodo, RefreshCw, SearchX, type LucideIcon } from "lucide-react";

import { Button, Card, CardContent, EmptyState, Skeleton } from "../../components/ui";
import { ApiError } from "../../lib/api";

/** Human-safe error text for the list; 422 fields get an explicit explanation. */
export function describeIssueListError(error: unknown): {
  title: string;
  message: string;
  code: string | null;
} {
  if (error instanceof ApiError) {
    if (error.status === 422) {
      const fields = error.fields ? Object.entries(error.fields) : [];
      const detail = fields.map(([field, message]) => `${field}：${message}`).join("；");
      return {
        title: "筛选条件无效",
        message: detail
          ? `服务器拒绝了以下参数：${detail}。请调整或清除筛选后重试。`
          : "服务器拒绝了当前筛选条件，请调整或清除筛选后重试。",
        code: error.code,
      };
    }
    return {
      title: "无法加载问题单",
      message: error.message || "请求失败",
      code: error.code,
    };
  }
  if (error instanceof Error && error.message) {
    return { title: "无法加载问题单", message: error.message, code: null };
  }
  return { title: "无法加载问题单", message: "请求失败，请稍后重试", code: null };
}

/** Initial-load skeleton; the parent status text is the screen-reader channel. */
export function IssueListSkeleton() {
  return (
    <div role="status" aria-busy="true" className="space-y-3">
      <span className="sr-only">正在加载问题单</span>
      <Card>
        <CardContent className="space-y-4 p-4">
          {Array.from({ length: 6 }, (_, index) => (
            <div key={index} className="flex items-center gap-4">
              <Skeleton rounded="full" className="size-5 shrink-0" />
              <div className="flex-1 space-y-2">
                <Skeleton className="h-4 w-2/5" />
                <Skeleton className="h-3 w-1/4" />
              </div>
              <Skeleton className="h-6 w-20" />
              <Skeleton className="hidden h-4 w-24 sm:block" />
              <Skeleton className="hidden h-4 w-16 md:block" />
            </div>
          ))}
        </CardContent>
      </Card>
    </div>
  );
}

interface IssueListErrorProps {
  error: unknown;
  onRetry: () => void;
}

/**
 * Background-refetch failure while cached rows are still on screen. The full
 * error detail (including 422 `fields`) stays visible; only the retry button
 * sits outside the live region so an interactive control is not announced as
 * status text.
 */
export function IssueListStaleNotice({ error, onRetry }: IssueListErrorProps) {
  const { title, message, code } = describeIssueListError(error);
  return (
    <div className="flex flex-wrap items-start justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text">
      <div role="status" className="min-w-0 flex-1">
        <p className="font-medium">问题单更新失败，正在显示上次数据。</p>
        <p className="mt-0.5 text-text-secondary">
          {title}：{message}
        </p>
        {code ? (
          <p className="mt-1 font-mono text-caption text-text-muted">{code}</p>
        ) : null}
      </div>
      <Button variant="ghost" size="sm" onClick={onRetry}>
        <RefreshCw aria-hidden="true" className="size-3.5" />
        重试
      </Button>
    </div>
  );
}

export function IssueListError({ error, onRetry }: IssueListErrorProps) {
  const { title, message, code } = describeIssueListError(error);
  return (
    <Card className="border-danger/40">
      <CardContent className="flex flex-wrap items-start gap-3 p-4">
        {/* The live region carries the message only; the retry button stays
            outside so it is not re-announced or swallowed by the alert. */}
        <div role="alert" className="flex min-w-0 flex-1 items-start gap-3">
          <AlertCircle aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-danger" />
          <div className="min-w-0 flex-1">
            <p className="text-body font-medium text-text">{title}</p>
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

export type IssueListEmptyKind = "no-data" | "filtered" | "page";

interface EmptyCopy {
  icon: LucideIcon;
  title: string;
  description: string;
  actionLabel: string | null;
}

function emptyCopy(kind: IssueListEmptyKind, total: number): EmptyCopy {
  switch (kind) {
    case "filtered":
      return {
        icon: SearchX,
        title: "筛选无结果",
        description: "没有符合当前筛选条件的问题单，请调整条件或清除筛选。",
        actionLabel: "清除筛选条件",
      };
    case "page":
      return {
        icon: FileQuestion,
        title: "该页没有数据",
        description: `共 ${total} 条问题单，当前页超出范围。回到第一页继续查看。`,
        actionLabel: "回到第一页",
      };
    default:
      return {
        icon: ListTodo,
        title: "暂无问题单",
        description: "运行器上报信号后，问题单会出现在这里。",
        actionLabel: null,
      };
  }
}

interface IssueListEmptyProps {
  kind: IssueListEmptyKind;
  total?: number;
  onAction?: () => void;
}

/** Distinguishes "no data at all" from "filters matched nothing". */
export function IssueListEmpty({ kind, total = 0, onAction }: IssueListEmptyProps) {
  const copy = emptyCopy(kind, total);
  return (
    <EmptyState
      icon={copy.icon}
      title={copy.title}
      description={copy.description}
      action={
        copy.actionLabel && onAction ? (
          <Button variant="secondary" size="sm" onClick={onAction}>
            {copy.actionLabel}
          </Button>
        ) : undefined
      }
    />
  );
}
