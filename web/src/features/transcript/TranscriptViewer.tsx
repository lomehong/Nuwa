import { ChevronRight, MessageSquareText } from "lucide-react";
import { useEffect, useRef } from "react";

import { Badge, Button, Skeleton, type BadgeVariant } from "../../components/ui";
import { ApiError } from "../../lib/api";
import { isoDateTime } from "../../lib/time";
import {
  formatAge,
  formatToolArguments,
  isLongText,
  messageTimeTitle,
  previewText,
  transcriptRoleLabel,
} from "./format";
import type {
  TranscriptBlock,
  TranscriptMessage,
  TranscriptResponse,
  TranscriptRole,
} from "./types";

const ROLE_VARIANTS: Record<TranscriptRole, BadgeVariant> = {
  user: "info",
  assistant: "accent",
  tool: "neutral",
};

const DETAIL_PRE_CLASS =
  "max-h-80 overflow-auto rounded-sm bg-raised/60 p-2 font-mono text-caption break-words whitespace-pre-wrap text-text-secondary";

const SUMMARY_CLASS =
  "flex min-h-11 cursor-pointer list-none items-center gap-2 px-3 py-2 text-body-sm font-medium text-text-secondary transition-colors hover:text-text [&::-webkit-details-marker]:hidden";

function DisclosureChevron() {
  return (
    <span
      aria-hidden="true"
      className="font-mono text-text-muted transition-transform duration-150 group-open:rotate-90"
    >
      <ChevronRight className="size-3.5" />
    </span>
  );
}

function TruncatedBadge() {
  return <Badge variant="warning">已截断</Badge>;
}

function TruncatedNote() {
  return (
    <p className="mt-1 text-caption text-text-muted">
      内容超过服务端 16KB 上限，仅显示前段；完整内容保存在运行器上的会话文件中。
    </p>
  );
}

function TextBlockView({ text, truncated }: { text: string; truncated: boolean }) {
  const body = text || "（空内容）";
  if (!isLongText(body)) {
    return (
      <div>
        <p className="text-body-sm break-words whitespace-pre-wrap text-text">{body}</p>
        {truncated ? (
          <div className="mt-1 flex flex-wrap items-center gap-1.5">
            <TruncatedBadge />
            <span className="text-caption text-text-muted">仅显示前 16KB。</span>
          </div>
        ) : null}
      </div>
    );
  }

  return (
    <div>
      <p className="text-body-sm break-words whitespace-pre-wrap text-text">
        {previewText(body)}
      </p>
      <details className="group mt-1 rounded-card border border-border bg-surface">
        <summary className={SUMMARY_CLASS}>
          <DisclosureChevron />
          展开全文（{body.length} 字符）
          {truncated ? <TruncatedBadge /> : null}
        </summary>
        <div className="border-t border-border p-2">
          <pre className={DETAIL_PRE_CLASS}>{body}</pre>
          {truncated ? <TruncatedNote /> : null}
        </div>
      </details>
    </div>
  );
}

function ThinkingBlockView({ text, truncated }: { text: string; truncated: boolean }) {
  return (
    <details className="group rounded-card border border-warning/40 bg-warning/5">
      <summary className={SUMMARY_CLASS}>
        <DisclosureChevron />
        思考过程
        <span className="font-normal text-text-muted">（默认折叠）</span>
        {truncated ? <TruncatedBadge /> : null}
      </summary>
      <div className="border-t border-warning/30 p-2">
        <pre className={DETAIL_PRE_CLASS}>{text || "（空思考）"}</pre>
        {truncated ? <TruncatedNote /> : null}
      </div>
    </details>
  );
}

function ToolCallBlockView({
  name,
  args,
  truncated,
}: {
  name: string;
  args: unknown;
  truncated: boolean;
}) {
  return (
    <div className="rounded-card border border-info/40 bg-info/5 p-2.5">
      <div className="flex flex-wrap items-center gap-1.5">
        <Badge variant="info">工具调用</Badge>
        <code className="font-mono text-caption font-medium text-text">{name || "未命名工具"}</code>
        {truncated ? <Badge variant="warning">参数已截断</Badge> : null}
      </div>
      <pre className={`mt-1.5 ${DETAIL_PRE_CLASS}`}>{formatToolArguments(args)}</pre>
      {truncated ? (
        <p className="mt-1 text-caption text-text-muted">
          参数体积过大，服务器只保留了预览字符串。
        </p>
      ) : null}
    </div>
  );
}

function ToolResultBlockView({
  name,
  text,
  isError,
  truncated,
}: {
  name: string;
  text: string;
  isError: boolean;
  truncated: boolean;
}) {
  return (
    <details className="group rounded-card border border-border bg-raised/30">
      <summary className={SUMMARY_CLASS}>
        <DisclosureChevron />
        工具结果 · {name || "未知工具"}
        <Badge variant={isError ? "danger" : "success"}>{isError ? "错误" : "正常"}</Badge>
        {truncated ? <TruncatedBadge /> : null}
      </summary>
      <div className="border-t border-border p-2">
        <pre className={DETAIL_PRE_CLASS}>{text || "（空结果）"}</pre>
        {truncated ? <TruncatedNote /> : null}
      </div>
    </details>
  );
}

function BlockView({ block }: { block: TranscriptBlock }) {
  switch (block.type) {
    case "thinking":
      return <ThinkingBlockView text={block.text} truncated={block.truncated === true} />;
    case "text":
      return <TextBlockView text={block.text} truncated={block.truncated === true} />;
    case "tool_call":
      return (
        <ToolCallBlockView
          name={block.name}
          args={block.arguments}
          truncated={block.truncated === true}
        />
      );
    case "tool_result":
      return (
        <ToolResultBlockView
          name={block.name}
          text={block.text}
          isError={block.is_error}
          truncated={block.truncated === true}
        />
      );
    default:
      return null;
  }
}

function MessageCard({ message }: { message: TranscriptMessage }) {
  return (
    <div className="rounded-card border border-border bg-surface p-3">
      <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-caption text-text-muted">
        <span className="font-mono">#{message.seq}</span>
        <time
          dateTime={isoDateTime(message.ts_ms)}
          title={messageTimeTitle(message.ts_ms)}
        >
          {formatAge(message.ts_ms)}
        </time>
      </div>
      <ul className="space-y-2" aria-label={`消息 #${message.seq} 的内容`}>
        {message.blocks.map((block, index) => (
          <li key={`${message.seq}-${block.type}-${index}`}>
            <BlockView block={block} />
          </li>
        ))}
      </ul>
    </div>
  );
}

interface RoleGroup {
  role: TranscriptRole;
  messages: TranscriptMessage[];
}

/** Consecutive messages with the same role share one heading. */
function groupMessages(messages: TranscriptMessage[]): RoleGroup[] {
  const groups: RoleGroup[] = [];
  for (const message of messages) {
    const last = groups[groups.length - 1];
    if (last && last.role === message.role) {
      last.messages.push(message);
    } else {
      groups.push({ role: message.role, messages: [message] });
    }
  }
  return groups;
}

function RoleGroups({ messages }: { messages: TranscriptMessage[] }) {
  const groups = groupMessages(messages);
  return (
    <div className="space-y-4">
      {groups.map((group, index) => {
        const headingId = `transcript-role-${index}`;
        return (
          <section key={`${group.role}-${index}`} aria-labelledby={headingId}>
            <div className="mb-2 flex flex-wrap items-center gap-2">
              <h4 id={headingId} className="text-body font-medium text-text">
                {transcriptRoleLabel(group.role)}消息
              </h4>
              <Badge variant={ROLE_VARIANTS[group.role]}>{transcriptRoleLabel(group.role)}</Badge>
              <span className="text-caption text-text-muted">{group.messages.length} 条</span>
            </div>
            <div className="space-y-2">
              {group.messages.map((message, messageIndex) => (
                <MessageCard key={`${message.seq}-${messageIndex}`} message={message} />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}

function TranscriptSkeleton() {
  return (
    <div role="status" aria-busy="true" className="space-y-3">
      <span className="sr-only">正在加载会话转录</span>
      {Array.from({ length: 4 }, (_, index) => (
        <div key={index} className="space-y-2 rounded-card border border-border p-3">
          <Skeleton className="h-3 w-24" />
          <Skeleton className="h-4 w-full" />
          <Skeleton className="h-4 w-3/4" />
        </div>
      ))}
    </div>
  );
}

function errorCopy(error: unknown, runId: string | null): { title: string; message: string; code: string | null } {
  if (error instanceof ApiError) {
    if (error.status === 404 && runId) {
      return {
        title: "所选运行没有会话转录",
        message: "该运行不存在于当前问题单，或运行没有产生可读取的会话文件。",
        code: error.code,
      };
    }
    return {
      title: "无法加载会话转录",
      message: error.message || "请求失败",
      code: error.code,
    };
  }
  if (error instanceof Error && error.message) {
    return { title: "无法加载会话转录", message: error.message, code: null };
  }
  return { title: "无法加载会话转录", message: "请求失败，请稍后重试", code: null };
}

export interface TranscriptViewerProps {
  response: TranscriptResponse | undefined;
  /** Initial load (no data yet). */
  isPending: boolean;
  isFetching: boolean;
  error: unknown;
  runId: string | null;
  page: number;
  onPageChange: (page: number) => void;
  onRetry: () => void;
  onClearRun: () => void;
}

/**
 * Transcript viewer (docs/09 §10.5): role-grouped messages, thinking and tool
 * results collapsed by default, long text expandable, server truncation and
 * page-size notices, pagination and error/empty/loading states.
 */
export function TranscriptViewer({
  response,
  isPending,
  isFetching,
  error,
  runId,
  page,
  onPageChange,
  onRetry,
  onClearRun,
}: TranscriptViewerProps) {
  const pageHeadingRef = useRef<HTMLHeadingElement>(null);
  const previousPage = useRef(page);

  // After a page change settles, focus a heading so keyboard/screen-reader
  // users land at the top of the new message block instead of at the footer.
  useEffect(() => {
    if (previousPage.current === page || isFetching) {
      return;
    }
    previousPage.current = page;
    pageHeadingRef.current?.focus();
  }, [page, isFetching]);

  if (isPending) {
    return <TranscriptSkeleton />;
  }

  if (error && !response) {
    const copy = errorCopy(error, runId);
    const missingRun = error instanceof ApiError && error.status === 404 && runId !== null;
    return (
      <div className="rounded-card border border-danger/40 bg-danger/10 p-4">
        {/* The live region carries the message only; the recovery actions stay
            outside it so they keep their own focus and labels. */}
        <div role="alert">
          <p className="text-body font-medium text-text">{copy.title}</p>
          <p className="mt-0.5 text-body-sm text-text-secondary">{copy.message}</p>
          {copy.code ? (
            <p className="mt-1 font-mono text-caption text-text-muted">{copy.code}</p>
          ) : null}
        </div>
        <div className="mt-3 flex flex-wrap gap-2">
          {missingRun ? (
            <Button variant="secondary" size="sm" onClick={onClearRun}>
              查看最近会话
            </Button>
          ) : null}
          <Button variant="ghost" size="sm" onClick={onRetry}>
            重试
          </Button>
        </div>
      </div>
    );
  }

  if (!response) {
    return (
      <p role="status" className="text-body-sm text-text-secondary">
        暂无会话数据。
      </p>
    );
  }

  const meta = response.meta;
  const totalPages = Math.max(1, Math.ceil(meta.total / Math.max(1, meta.page_size)));
  const pageOutOfRange = meta.total > 0 && response.data.length === 0 && page > totalPages;

  return (
    <div className="space-y-4" aria-busy={isFetching || undefined}>
      <h3
        ref={pageHeadingRef}
        tabIndex={-1}
        data-testid="transcript-page-heading"
        className="sr-only"
      >
        会话消息 · 第 {page} / {totalPages} 页
      </h3>

      {meta.truncated ? (
        <p
          role="status"
          className="rounded-card border border-warning/40 bg-warning/10 px-3 py-2 text-caption text-text"
        >
          会话文件超过 2MB，服务器仅解析了末尾内容；更早的消息未包含在内。
        </p>
      ) : null}

      {error && response ? (
        <div className="flex flex-wrap items-start justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-3 py-2 text-caption text-text">
          <span role="status">会话更新失败，显示的是上次数据。</span>
          <Button variant="ghost" size="sm" onClick={onRetry}>
            重试
          </Button>
        </div>
      ) : null}

      {response.data.length > 0 ? (
        <RoleGroups messages={response.data} />
      ) : pageOutOfRange ? (
        <div className="space-y-2">
          <p className="text-body font-medium text-text">该页没有消息</p>
          <p className="text-body-sm text-text-secondary">
            会话共 {meta.total} 条消息，当前页超出范围。
          </p>
          <Button variant="secondary" size="sm" onClick={() => onPageChange(1)}>
            回到第一页
          </Button>
        </div>
      ) : (
        <div className="flex flex-col items-center gap-2 rounded-card border border-dashed border-border px-4 py-8 text-center">
          {/* Announce the empty result without swallowing the surrounding
              context; no action lives inside the live region. */}
          <div role="status" className="flex flex-col items-center gap-2">
            <MessageSquareText
              aria-hidden="true"
              className="size-5 text-text-muted"
            />
            <p className="text-body font-medium text-text">暂无会话转录</p>
            <p className="max-w-prose text-body-sm text-text-secondary">
              {runId
                ? "所选运行没有可显示的会话消息，可能未产生转录文件。"
                : "该问题单还没有可读取的会话转录。修复运行进行中时此视图会自动刷新；结束后可重试。"}
            </p>
          </div>
        </div>
      )}

      {meta.total > 0 ? (
        <nav
          aria-label="会话分页"
          className="flex flex-wrap items-center justify-between gap-2 border-t border-border pt-3"
        >
          <p className="text-caption text-text-secondary">
            第 {page} / {totalPages} 页 · 共 {meta.total} 条消息 · 每页 {meta.page_size} 条
          </p>
          <div className="flex items-center gap-2">
            <Button
              variant="secondary"
              size="sm"
              disabled={page <= 1}
              onClick={() => onPageChange(1)}
            >
              首页
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={page <= 1}
              onClick={() => onPageChange(page - 1)}
            >
              上一页
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => onPageChange(page + 1)}
            >
              下一页
            </Button>
            {/* Messages stream in ascending order, so the last page tracks the
                live tail of an in-flight run (docs/09 §10.5). */}
            <Button
              variant="secondary"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => onPageChange(totalPages)}
            >
              末页（最新）
            </Button>
          </div>
        </nav>
      ) : null}

      <p className="text-caption text-text-muted">
        单块内容超过 16KB 时由服务端截断；更长的消息在查看器中默认折叠。
      </p>
    </div>
  );
}
