import { Link } from "react-router-dom";

import { Card, CardContent, EmptyState, SeverityStone, Skeleton } from "../../components/ui";
import { FolderTree } from "lucide-react";
import { formatLastSignal, projectLabel } from "./format";
import { useProjectsQuery } from "./queries";

/**
 * 跨项目分诊 — the first thing the overview shows.
 *
 * The operator's question is never "which issue first" but "which project is
 * degrading". Every project gets one row with the numbers that decide whether
 * to open it.
 */
export function ProjectTriage() {
  const projects = useProjectsQuery();

  if (projects.isPending) {
    return (
      <div className="space-y-2" role="status" aria-busy="true">
        <span className="sr-only">正在加载项目分诊</span>
        <Skeleton className="h-4 w-24" />
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
      </div>
    );
  }

  const data = projects.data?.data ?? [];
  const top = data.slice(0, 6);

  return (
    <section aria-labelledby="project-triage" className="space-y-2">
      <header className="flex items-baseline justify-between gap-3">
        <h3 id="project-triage" className="text-heading-sm font-semibold text-text">
          项目分诊
        </h3>
        <Link
          to="/projects"
          className="text-caption text-text-secondary transition-colors hover:text-accent"
        >
          项目台账
        </Link>
      </header>

      {top.length === 0 ? (
        <Card>
          <CardContent className="p-4">
            <EmptyState
              icon={FolderTree}
              title="还没有项目"
              description="采集端上报第一条信号后，项目会自动出现在这里。"
            />
          </CardContent>
        </Card>
      ) : (
        <div className="ledger rounded-card border border-border bg-surface">
          <div className="ledger__row hidden grid-cols-[minmax(0,1fr)_2.5rem_2.5rem_2.5rem_2.5rem_5rem] items-center gap-3 px-3 py-1.5 text-caption text-text-muted md:grid">
            <span>项目</span>
            <span className="text-right">需处理</span>
            <span className="text-right">队列</span>
            <span className="text-right">运行中</span>
            <span className="text-right">PR</span>
            <span className="text-right">最近信号</span>
          </div>
          {top.map((project) => (
            <Link
              key={project.repo}
              to={`/projects/${encodeURIComponent(project.repo)}`}
              className="ledger__row grid grid-cols-[minmax(0,1fr)_2.5rem_2.5rem_2.5rem_2.5rem_5rem] items-center gap-3 px-3 py-2 transition-colors hover:bg-raised"
            >
              <span className="flex min-w-0 items-center gap-2">
                <span className="flex size-5 shrink-0 items-center justify-center">
                  {project.worst_severity ? (
                    <SeverityStone
                      severity={project.worst_severity}
                      hideLabel
                      className="size-3.5"
                    />
                  ) : (
                    <span aria-hidden="true" className="size-1.5 rounded-full bg-border-strong" />
                  )}
                </span>
                <span className="min-w-0">
                  <span className="block truncate text-body-sm text-text">
                    {projectLabel(project.name, project.repo)}
                  </span>
                  <span className="block truncate font-mono text-caption text-text-muted">
                    {project.repo}
                  </span>
                </span>
              </span>
              <span className="ledger__num text-right text-body-sm text-text-secondary">
                {project.attention > 0 ? (
                  <span className="font-medium text-danger">{project.attention}</span>
                ) : (
                  "—"
                )}
              </span>
              <span className="ledger__num text-right text-body-sm text-text-secondary">
                {project.queued}
              </span>
              <span className="ledger__num text-right text-body-sm text-text-secondary">
                {project.running}
              </span>
              <span className="ledger__num text-right text-body-sm text-text-secondary">
                {project.pr_open}
              </span>
              <span className="ledger__num text-right text-caption whitespace-nowrap text-text-muted">
                {formatLastSignal(project.last_signal_ms)}
              </span>
            </Link>
          ))}
        </div>
      )}
    </section>
  );
}
