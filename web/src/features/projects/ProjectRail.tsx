import { clsx } from "clsx";
import { NavLink } from "react-router-dom";

import { SeverityStone } from "../../components/ui";
import { projectLabel } from "./format";
import type { ProjectSummary } from "./types";

/** Route of a project's workbench. */
export function projectPath(repo: string): string {
  return `/projects/${encodeURIComponent(repo)}`;
}

interface ProjectRailProps {
  projects: ProjectSummary[];
  /** Repository of the page currently in view, if any. */
  activeRepo?: string | null;
}

/**
 * The project rail: one nameplate per project, ordered by what needs a human.
 *
 * This is the console's primary navigation. A nameplate carries the project's
 * worst open severity (the 五色石 mark), its name, its full identity in mono
 * and the one number that decides whether to look: how many issues need the
 * owner. Everything else lives on the project's own page.
 */
export function ProjectRail({ projects, activeRepo }: ProjectRailProps) {
  if (projects.length === 0) {
    return (
      <p className="px-3 py-2 text-caption text-text-muted">
        还没有项目。第一条信号到达后会自动登记到这里。
      </p>
    );
  }
  return (
    <ul className="space-y-px">
      {projects.map((project) => {
        const active = project.repo === activeRepo;
        const label = projectLabel(project.name, project.repo);
        return (
          <li key={project.repo}>
            <NavLink
              to={projectPath(project.repo)}
              title={`${label} · ${project.repo}`}
              className={({ isActive }) =>
                clsx(
                  "nameplate flex min-h-11 items-center gap-2.5 rounded-card px-2 py-1.5 transition-colors",
                  isActive || active
                    ? "nameplate--active bg-accent/10"
                    : "hover:bg-raised",
                )
              }
            >
              <span className="flex size-6 shrink-0 items-center justify-center">
                {project.worst_severity ? (
                  <SeverityStone
                    severity={project.worst_severity}
                    hideLabel
                    className="size-4"
                  />
                ) : (
                  <span
                    aria-hidden="true"
                    className="size-1.5 rounded-full bg-border-strong"
                  />
                )}
              </span>
              <span className="min-w-0 flex-1">
                <span
                  className={clsx(
                    "block truncate text-body-sm font-medium",
                    active ? "text-text" : "text-text-secondary",
                  )}
                >
                  {label}
                </span>
                <span
                  className="block truncate font-mono text-caption text-text-muted"
                  title={project.repo}
                >
                  {project.repo}
                </span>
              </span>
              {project.attention > 0 ? (
                <span className="ledger__num shrink-0 text-caption font-medium text-danger">
                  {project.attention}
                </span>
              ) : project.queued > 0 ? (
                <span className="ledger__num shrink-0 text-caption text-text-muted">
                  {project.queued}
                </span>
              ) : null}
            </NavLink>
          </li>
        );
      })}
    </ul>
  );
}
