import { clsx } from "clsx";
import { FolderTree, Plus } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router-dom";

import {
  Button,
  Card,
  CardContent,
  ConfirmDialog,
  EmptyState,
  SeverityStone,
  Skeleton,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableRow,
  useToast,
} from "../../components/ui";
import { describeError } from "../../lib/errors";
import { formatLastSignal, projectLabel } from "./format";
import { useProjectsQuery } from "./queries";
import type { ProjectSummary } from "./types";
import { RepoFormDialog } from "./RepoFormDialog";
import { useDeleteRepoMutation, useReposQuery, type RepoDto } from "./registry";

function Cell({ value, tone }: { value: number; tone?: "danger" | "muted" }) {
  return (
    <span
      className={clsx(
        "ledger__num text-body-sm",
        tone === "danger" && value > 0 ? "text-danger" : "text-text-secondary",
        tone === "muted" ? "text-text-muted" : "",
      )}
    >
      {value}
    </span>
  );
}

function ProjectName({
  project,
  linked = true,
}: {
  project: ProjectSummary;
  /** False inside a link: a link may not nest another link. */
  linked?: boolean;
}) {
  const label = projectLabel(project.name, project.repo);
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-2">
        {project.worst_severity ? (
          <SeverityStone severity={project.worst_severity} hideLabel className="size-3.5" />
        ) : null}
        {linked ? (
          <Link
            to={`/projects/${encodeURIComponent(project.repo)}`}
            className="truncate text-body-sm font-medium text-text transition-colors hover:text-accent"
          >
            {label}
          </Link>
        ) : (
          <span className="truncate text-body-sm font-medium text-text">{label}</span>
        )}
        {!project.registered ? (
          <span className="shrink-0 text-caption text-text-muted">未登记</span>
        ) : null}
      </div>
      <p className="truncate font-mono text-caption text-text-muted">{project.repo}</p>
    </div>
  );
}

/**
 * 项目台账 — every project the instance knows, ordered by what needs the
 * owner first. Registry entries without a heartbeat have an identity but no
 * signals, so they live below the table until the first runner reports.
 */
export function ProjectsPage() {
  const projects = useProjectsQuery();
  const repos = useReposQuery();
  const removeRepo = useDeleteRepoMutation();
  const { toast } = useToast();
  const [createOpen, setCreateOpen] = useState(false);
  const [editing, setEditing] = useState<RepoDto | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<RepoDto | null>(null);

  if (projects.isPending) {
    return (
      <div className="space-y-3" role="status" aria-busy="true">
        <span className="sr-only">正在加载项目台账</span>
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    );
  }

  const data = projects.data?.data ?? [];
  const known = new Set(data.map((project) => project.repo));
  const registeredOnly = (repos.data?.data ?? []).filter((repo) => !known.has(repo.repo));

  function handleRemove() {
    const target = deleteTarget;
    if (!target) {
      return;
    }
    removeRepo.mutate(target.repo, {
      onSuccess: () => {
        setDeleteTarget(null);
        toast({
          title: "已取消登记",
          description: `${target.name}（${target.repo}）`,
          variant: "success",
        });
      },
      onError: (error) => {
        setDeleteTarget(null);
        toast({
          title: "取消登记失败",
          description: describeError(error),
          variant: "danger",
        });
      },
    });
  }

  return (
    <section className="space-y-4">
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div className="space-y-1">
          <h2 className="text-title font-semibold tracking-tight text-text">项目台账</h2>
          <p className="text-body-sm text-text-secondary">
            {data.length > 0
              ? `${data.length} 个项目按“需要你处理的数量”排序。点开项目进入工位。`
              : "还没有项目。登记仓库或等待采集端上报第一条信号。"}
          </p>
        </div>
        <Button variant="primary" size="sm" onClick={() => setCreateOpen(true)}>
          <Plus aria-hidden="true" className="size-3.5" />
          登记仓库
        </Button>
      </header>

      {repos.isError ? (
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text">
          <p role="status" className="min-w-0 flex-1">
            仓库登记信息加载失败，已登记但未上报的项目可能不完整。
            <span className="ml-1 text-text-secondary">{describeError(repos.error)}</span>
          </p>
          <Button variant="secondary" size="sm" onClick={() => void repos.refetch()}>
            重试
          </Button>
        </div>
      ) : null}

      {data.length === 0 ? (
        <Card>
          <CardContent className="p-6">
            <EmptyState
              icon={FolderTree}
              title="还没有项目"
              description="采集端上报第一条信号后，项目会自动出现在这里。也可以先登记仓库，首拍心跳后进入台账。"
            />
          </CardContent>
        </Card>
      ) : (
        <>
          <Table
            caption="项目台账"
            label="项目台账"
            containerClassName="hidden md:block"
          >
            <TableHead>
              <TableRow>
                <TableHeaderCell>项目</TableHeaderCell>
                <TableHeaderCell className="text-right">需处理</TableHeaderCell>
                <TableHeaderCell className="text-right">队列</TableHeaderCell>
                <TableHeaderCell className="text-right">运行中</TableHeaderCell>
                <TableHeaderCell className="text-right">PR 待合并</TableHeaderCell>
                <TableHeaderCell className="text-right">运行器</TableHeaderCell>
                <TableHeaderCell className="text-right">UTC 今日确认 PR（记录）</TableHeaderCell>
                <TableHeaderCell className="text-right">UTC 准入槽位</TableHeaderCell>
                <TableHeaderCell className="text-right">CI 24h</TableHeaderCell>
                <TableHeaderCell>最近信号</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {data.map((project) => (
                <TableRow key={project.repo}>
                  <TableCell>
                    <ProjectName project={project} />
                  </TableCell>
                  <TableCell className="text-right">
                    <Cell value={project.attention} tone="danger" />
                  </TableCell>
                  <TableCell className="text-right">
                    <Cell value={project.queued} />
                  </TableCell>
                  <TableCell className="text-right">
                    <Cell value={project.running} />
                  </TableCell>
                  <TableCell className="text-right">
                    <Cell value={project.pr_open} />
                  </TableCell>
                  <TableCell className="text-right">
                    <span className="ledger__num text-body-sm text-text-secondary">
                      {project.runners_online}/{project.runners}
                    </span>
                  </TableCell>
                  <TableCell className="text-right">
                    <Cell value={project.prs_today} />
                  </TableCell>
                  <TableCell className="text-right">
                    <span title="含在途、结果不明及已消耗；重试保留原准入日">
                      <Cell value={project.pr_slots_today} />
                    </span>
                  </TableCell>
                  <TableCell className="text-right">
                    <span className="ledger__num text-body-sm text-text-secondary">
                      {project.ci_green}
                      <span className="mx-0.5 text-text-muted">/</span>
                      <span className={project.ci_red > 0 ? "text-danger" : "text-text-muted"}>
                        {project.ci_red}
                      </span>
                    </span>
                  </TableCell>
                  <TableCell>
                    <span className="text-body-sm whitespace-nowrap text-text-secondary">
                      {formatLastSignal(project.last_signal_ms)}
                    </span>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>

          <ul className="space-y-2 md:hidden">
            {data.map((project) => (
              <li key={project.repo}>
                <Link
                  to={`/projects/${encodeURIComponent(project.repo)}`}
                  className="nameplate flex min-h-11 items-center gap-3 rounded-card border border-border bg-surface px-3 py-2"
                >
                  <span className="min-w-0 flex-1">
                    <ProjectName project={project} linked={false} />
                  </span>
                  <span className="flex shrink-0 items-center gap-3">
                    {project.attention > 0 ? (
                      <span className="ledger__num text-body-sm font-medium text-danger">
                        {project.attention}
                      </span>
                    ) : null}
                    <span className="ledger__num text-body-sm text-text-muted">
                      {project.queued}
                    </span>
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        </>
      )}

      {registeredOnly.length > 0 ? (
        <section className="space-y-2" aria-label="已登记等待心跳">
          <div className="flex flex-wrap items-end justify-between gap-2">
            <div>
              <h3 className="text-body font-semibold text-text">已登记 · 等待首个心跳</h3>
              <p className="text-caption text-text-secondary">
                登记只建立展示身份；runner 首拍心跳后进入项目台账。
              </p>
            </div>
            <span className="ledger__num text-caption text-text-muted">
              {registeredOnly.length} 个
            </span>
          </div>
          <ul className="divide-y divide-border rounded-card border border-border bg-surface">
            {registeredOnly.map((repo) => (
              <li
                key={repo.repo}
                className="flex flex-wrap items-start justify-between gap-3 px-4 py-3"
              >
                <div className="min-w-0">
                  <p className="text-body-sm font-medium text-text">{repo.name}</p>
                  <p className="break-all font-mono text-caption text-text-muted">{repo.repo}</p>
                  <p className="break-all text-caption text-text-secondary">{repo.url}</p>
                  {repo.description ? (
                    <p className="break-words text-caption text-text-muted">{repo.description}</p>
                  ) : null}
                </div>
                <div className="flex shrink-0 flex-wrap gap-1">
                  <Button
                    variant="secondary"
                    size="sm"
                    aria-label={`编辑登记：${repo.name}`}
                    onClick={() => setEditing(repo)}
                  >
                    编辑登记
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-label={`取消登记：${repo.name}`}
                    onClick={() => setDeleteTarget(repo)}
                  >
                    取消登记
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <RepoFormDialog
        open={createOpen}
        mode="create"
        title="登记仓库"
        submitLabel="登记"
        onClose={() => setCreateOpen(false)}
      />

      <RepoFormDialog
        open={editing !== null}
        mode="edit"
        target={editing}
        title="编辑登记"
        onClose={() => setEditing(null)}
      />

      <ConfirmDialog
        open={deleteTarget !== null}
        title="取消这个仓库的登记？"
        description={
          deleteTarget
            ? `将从控制台移除 ${deleteTarget.name}（${deleteTarget.repo}）的登记。这不会停止 runner，也不会修改目标仓库的 nuwa.repo.json；仓库内容保持不变。`
            : undefined
        }
        confirmLabel="取消登记"
        cancelLabel="保留登记"
        danger
        pending={removeRepo.isPending}
        cancelDisabled={removeRepo.isPending}
        onConfirm={handleRemove}
        onCancel={() => {
          if (!removeRepo.isPending) {
            setDeleteTarget(null);
          }
        }}
      />
    </section>
  );
}
