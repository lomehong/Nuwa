import { clsx } from "clsx";
import { Link } from "react-router-dom";

import {
  Card,
  CardContent,
  EmptyState,
  SeverityStone,
  Skeleton,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableRow,
} from "../../components/ui";
import { formatLastSignal, projectLabel } from "./format";
import { useProjectsQuery } from "./queries";
import type { ProjectSummary } from "./types";
import { FolderTree } from "lucide-react";

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

function ProjectName({ project }: { project: ProjectSummary }) {
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-2">
        {project.worst_severity ? (
          <SeverityStone severity={project.worst_severity} hideLabel className="size-3.5" />
        ) : null}
        <Link
          to={`/projects/${encodeURIComponent(project.repo)}`}
          className="truncate text-body-sm font-medium text-text transition-colors hover:text-accent"
        >
          {projectLabel(project.name, project.repo)}
        </Link>
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
 * owner first. The table is the console's index: a project row answers "which
 * project is hurting, and how much" without opening anything.
 */
export function ProjectsPage() {
  const projects = useProjectsQuery();

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

  return (
    <section className="space-y-4">
      <header className="space-y-1">
        <h2 className="text-title font-semibold tracking-tight text-text">项目台账</h2>
        <p className="text-body-sm text-text-secondary">
          {data.length > 0
            ? `${data.length} 个项目按“需要你处理的数量”排序。点开项目进入工位。`
            : "还没有任何项目。"}
        </p>
      </header>

      {data.length === 0 ? (
        <Card>
          <CardContent className="p-6">
            <EmptyState
              icon={FolderTree}
              title="还没有项目"
              description="采集端上报第一条信号后，项目会自动出现在这里。"
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
                <TableHeaderCell className="text-right">今日 PR</TableHeaderCell>
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
                    <ProjectName project={project} />
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
    </section>
  );
}
