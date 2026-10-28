import { ArrowLeft, BrainCircuit, ChevronLeft, ChevronRight, RefreshCw, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";

import {
  Badge,
  Button,
  Card,
  CardContent,
  ConfirmDialog,
  EmptyState,
  Input,
  Select,
  Skeleton,
  StateBadge,
  useToast,
  type IssueState,
} from "../../components/ui";
import { describeError } from "../../lib/errors";
import { PrLink } from "../issues/IssueBadges";
import { formatDateTime } from "../overview/format";
import {
  MEMORY_PAGE_SIZE,
  useDeleteMemoryMutation,
  useProjectMemoryQuery,
  useSetMemoryDisabledMutation,
} from "./queries";
import type { MemoryDto } from "./types";

function shortCommit(commit: string): string {
  return commit.length > 12 ? commit.slice(0, 12) : commit;
}

function hasValidEvidence(entry: MemoryDto): boolean {
  return (
    entry.evidence_valid &&
    entry.revoked_at_ms === null &&
    (entry.verification_source === "local_verification" || entry.verification_source === "ci_green")
  );
}

function isPromptEligible(entry: MemoryDto): boolean {
  return !entry.disabled && hasValidEvidence(entry);
}

function stateBadge(state: string) {
  const values = [
    "new",
    "triggered",
    "fixing",
    "pr_open",
    "merged",
    "rejected",
    "escalated",
    "ignored",
  ] as const;
  if ((values as readonly string[]).includes(state)) {
    return <StateBadge state={state as IssueState} />;
  }
  return <Badge variant="neutral">{state || "问题单已清理"}</Badge>;
}

function MemoryCard({
  entry,
  onToggle,
  onDelete,
  pending,
}: {
  entry: MemoryDto;
  onToggle: (entry: MemoryDto) => void;
  onDelete: (entry: MemoryDto) => void;
  pending: boolean;
}) {
  return (
    <li>
      <Card>
        <CardContent className="space-y-3 p-4">
          <div className="flex flex-wrap items-start justify-between gap-2">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-1.5">
                {stateBadge(entry.state)}
                <Badge variant={entry.disabled ? "warning" : "neutral"}>
                  {entry.disabled ? "已停用" : "未停用"}
                </Badge>
                <Badge variant={hasValidEvidence(entry) ? "success" : "warning"}>
                  {entry.revoked_at_ms !== null
                    ? "证据已撤销"
                    : hasValidEvidence(entry)
                      ? "证据有效"
                      : entry.verification_source === "legacy_unknown"
                        ? "证据未验证"
                        : "证据无效"}
                </Badge>
                <Badge variant="info">{entry.files} 个文件</Badge>
              </div>
              <h3 className="mt-1 text-body font-medium break-words text-text">
                {entry.title || "（问题单已不在库中）"}
              </h3>
              <Link
                to={`/issues/${encodeURIComponent(entry.fingerprint)}`}
                className="mt-0.5 inline-block font-mono text-caption break-all text-text-muted transition-colors hover:text-accent"
              >
                {entry.fingerprint}
              </Link>
            </div>
            <div className="flex shrink-0 flex-wrap items-center gap-1.5">
              <Button
                variant="secondary"
                size="sm"
                onClick={() => onToggle(entry)}
                disabled={pending}
              >
                {entry.disabled ? "启用" : "停用"}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => onDelete(entry)}
                disabled={pending}
                aria-label={`删除记忆：${entry.fingerprint}`}
              >
                <Trash2 aria-hidden="true" className="size-3.5" />
              </Button>
            </div>
          </div>

          <p className="text-caption text-text-secondary">
            {isPromptEligible(entry) ? "可用于后续提示" : "不用于后续提示"}
          </p>

          <div>
            <p className="text-caption text-text-muted">根因</p>
            <pre className="mt-1 max-h-40 overflow-auto font-mono text-caption whitespace-pre-wrap text-text">
              {entry.root_cause || "—"}
            </pre>
          </div>

          <div className="grid gap-2 sm:grid-cols-2">
            <div className="min-w-0">
              <p className="text-caption text-text-muted">来源</p>
              <div className="mt-1 flex flex-wrap items-center gap-2 text-caption">
                <span>验证来源：{entry.verification_source}</span>
                <span className="font-mono" title={entry.commit_sha}>
                  commit {shortCommit(entry.commit_sha)}
                </span>
                {entry.pr_url ? <PrLink url={entry.pr_url} title={entry.fingerprint} /> : null}
                <span className="text-text-muted">
                  记录于 {formatDateTime(entry.recorded_at_ms)}
                </span>
              </div>
              {entry.revoked_at_ms !== null ? (
                <p className="mt-1 text-caption text-text-secondary">
                  撤销于 {formatDateTime(entry.revoked_at_ms)}
                </p>
              ) : null}
              {entry.revocation_reason ? (
                <p className="mt-1 text-caption break-words text-text-secondary">
                  撤销原因：{entry.revocation_reason}
                </p>
              ) : null}
            </div>
            <div className="min-w-0">
              <p className="text-caption text-text-muted">改动</p>
              <pre className="mt-1 max-h-24 overflow-auto font-mono text-caption whitespace-pre-wrap text-text-secondary">
                {entry.change_stat || "—"}
              </pre>
            </div>
          </div>
        </CardContent>
      </Card>
    </li>
  );
}

/**
 * 项目修复记忆：带验证证据与 commit/PR 来源的修复经验，包括历史及已撤销记录。
 *
 * 仅有效且未手动停用的记忆可进入提示；启用不恢复已撤销证据。
 */
export function ProjectMemoryPage() {
  const repo = useParams<{ repo: string }>().repo ?? "";
  const [page, setPage] = useState(1);
  const query = useProjectMemoryQuery(repo, page);
  const toggle = useSetMemoryDisabledMutation(repo);
  const remove = useDeleteMemoryMutation(repo);
  const { toast } = useToast();
  const [filter, setFilter] = useState<"all" | "active" | "disabled" | "invalid">("all");
  const [search, setSearch] = useState("");
  const [pendingDelete, setPendingDelete] = useState<MemoryDto | null>(null);

  const rows = query.data?.data ?? [];
  const filtered = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return rows.filter((entry) => {
      if (filter === "active" && !isPromptEligible(entry)) return false;
      if (filter === "disabled" && !entry.disabled) return false;
      if (filter === "invalid" && hasValidEvidence(entry)) return false;
      if (!needle) return true;
      return [entry.fingerprint, entry.title, entry.root_cause, entry.change_stat]
        .join("\n")
        .toLowerCase()
        .includes(needle);
    });
  }, [rows, filter, search]);

  const total = query.data?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(total / MEMORY_PAGE_SIZE));

  function handleToggle(entry: MemoryDto) {
    toggle.mutate(
      { fingerprint: entry.fingerprint, disabled: !entry.disabled },
      {
        onSuccess: () => {
          toast({
            title: entry.disabled ? "已清除手动停用" : "已停用记忆",
            description:
              entry.disabled && !hasValidEvidence(entry)
                ? "仅清除手动停用；无效或已撤销的证据仍不会用于提示。"
                : entry.fingerprint,
            variant: "success",
          });
        },
        onError: (error) => {
          toast({ title: "操作失败", description: describeError(error), variant: "danger" });
        },
      },
    );
  }

  function handleDelete() {
    if (!pendingDelete) return;
    const target = pendingDelete;
    remove.mutate(target.fingerprint, {
      onSuccess: () => {
        setPendingDelete(null);
        toast({ title: "已删除记忆", description: target.fingerprint, variant: "success" });
      },
      onError: (error) => {
        setPendingDelete(null);
        toast({ title: "删除失败", description: describeError(error), variant: "danger" });
      },
    });
  }

  return (
    <section className="space-y-5">
      <header className="space-y-3">
        <Link
          to={`/projects/${encodeURIComponent(repo)}`}
          className="inline-flex min-h-11 items-center gap-1.5 text-body-sm text-text-secondary transition-colors hover:text-accent"
        >
          <ArrowLeft aria-hidden="true" className="size-4" />
          返回项目
        </Link>
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <h2 className="flex items-center gap-2 text-title font-semibold tracking-tight text-text">
              <BrainCircuit aria-hidden="true" className="size-5 text-accent" />
              修复记忆
            </h2>
            <p className="mt-0.5 font-mono text-caption break-all text-text-muted">{repo}</p>
            <p className="mt-1 max-w-3xl text-body-sm text-text-secondary">
              记忆保留验证来源与 commit/PR 证据，包括未验证的历史记录和已撤销的证据。
              只有证据有效且未手动停用的记忆可进入后续修复提示；它只是参考，不是权威。
              重新启用只清除手动停用，不会恢复已撤销证据，也不会让未验证的历史记录生效。
            </p>
          </div>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void query.refetch()}
            disabled={query.isFetching}
          >
            <RefreshCw aria-hidden="true" className="size-3.5" />
            {query.isFetching ? "刷新中" : "刷新"}
          </Button>
        </div>
      </header>

      <Card>
        <CardContent className="grid gap-4 p-4 sm:grid-cols-3">
          <div>
            <p className="text-caption text-text-muted">记忆总数</p>
            <p className="mt-1 text-body font-medium text-text">{total}</p>
            <p className="mt-1 text-caption text-text-secondary">每仓最多保留 50 条</p>
          </div>
          <div>
            <p className="text-caption text-text-muted">本页可用于提示 / 手动停用</p>
            <p className="mt-1 text-body font-medium text-text">
              {rows.filter(isPromptEligible).length} /{" "}
              {rows.filter((entry) => entry.disabled).length}
            </p>
            <p className="mt-1 text-caption text-text-secondary">
              证据无效 {rows.filter((entry) => !hasValidEvidence(entry)).length} 条；停用不会删除来源证据
            </p>
          </div>
          <div>
            <p className="text-caption text-text-muted">维护方式</p>
            <p className="mt-1 text-body font-medium text-text">自动写入</p>
            <p className="mt-1 text-caption text-text-secondary">
              模型自述不会进记忆，只有门禁证据会
            </p>
          </div>
        </CardContent>
      </Card>

      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        <Input
          label="搜索记忆"
          placeholder="问题单、根因或改动"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
        <Select
          label="状态"
          value={filter}
          onChange={(event) =>
            setFilter(event.target.value as "all" | "active" | "disabled" | "invalid")
          }
          options={[
            { value: "all", label: "全部" },
            { value: "active", label: "生效中" },
            { value: "disabled", label: "已停用" },
            { value: "invalid", label: "证据无效" },
          ]}
        />
        <div className="flex items-end justify-between gap-2 text-caption text-text-muted">
          <span>当前页 {rows.length} 条</span>
          <span>
            第 {page} / {totalPages} 页
          </span>
        </div>
      </div>

      {query.isPending ? (
        <div className="space-y-3" role="status" aria-busy="true">
          <span className="sr-only">正在加载修复记忆</span>
          <Skeleton className="h-32 w-full" />
          <Skeleton className="h-32 w-full" />
        </div>
      ) : query.isError ? (
        <Card>
          <CardContent className="p-6">
            <p className="text-body-sm text-danger">记忆加载失败：{describeError(query.error)}</p>
          </CardContent>
        </Card>
      ) : filtered.length === 0 ? (
        <Card>
          <CardContent className="p-6">
            <EmptyState
              icon={BrainCircuit}
              title={total === 0 ? "还没有修复记忆" : "没有匹配的记忆"}
              description={
                total === 0
                  ? "当同仓问题单通过验证门与 CI 后，runner 会自动写入；无需手工维护。"
                  : "调整搜索或状态筛选。"
              }
            />
          </CardContent>
        </Card>
      ) : (
        <ul className="space-y-3" aria-label="修复记忆列表">
          {filtered.map((entry) => (
            <MemoryCard
              key={entry.fingerprint}
              entry={entry}
              onToggle={handleToggle}
              onDelete={setPendingDelete}
              pending={toggle.isPending || remove.isPending}
            />
          ))}
        </ul>
      )}

      <div className="flex flex-wrap items-center justify-between gap-2">
        <Button
          variant="secondary"
          size="sm"
          onClick={() => setPage((value) => Math.max(1, value - 1))}
          disabled={page <= 1 || query.isFetching}
        >
          <ChevronLeft aria-hidden="true" className="size-4" />
          上一页
        </Button>
        <p className="text-caption text-text-muted">
          共 {total} 条 · 每页 {MEMORY_PAGE_SIZE} 条 · 本轮注入最多取最近 3 条
        </p>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => setPage((value) => Math.min(totalPages, value + 1))}
          disabled={page >= totalPages || query.isFetching}
        >
          下一页
          <ChevronRight aria-hidden="true" className="size-4" />
        </Button>
      </div>

      <p className="text-caption text-text-muted">
        提示：记忆只注入同仓其他问题单；当前问题单的历史在 PRIOR ATTEMPTS 中。
        运行中的修复不受此刻停用影响，下一轮起生效。
      </p>

      <ConfirmDialog
        open={pendingDelete !== null}
        title="删除这条修复记忆？"
        description={
          pendingDelete
            ? `${pendingDelete.fingerprint} 的来源与内容会一并删除，且不再注入后续提示。`
            : undefined
        }
        confirmLabel="删除"
        danger
        pending={remove.isPending}
        onConfirm={handleDelete}
        onCancel={() => setPendingDelete(null)}
      />
    </section>
  );
}
