/**
 * Project (repository under repair) shapes for `GET /api/v1/projects`.
 *
 * A project is the console's primary object: one row per repository with the
 * numbers an owner triages on. Fields come from the Rust DTO via ts-rs; only
 * `worst_severity` is narrowed to the UI's severity union.
 */

import type { ProjectDto } from "../../lib/api-types/ProjectDto";
import type { ProjectsResponse as ApiProjectsResponse } from "../../lib/api-types/ProjectsResponse";
import type { Severity } from "../../components/ui";

export type ProjectSummary = Omit<ProjectDto, "worst_severity"> & {
  /** Highest severity among the project's open issues — the stone mark. */
  worst_severity: Severity | null;
};

export type ProjectsResponse = Omit<ApiProjectsResponse, "data"> & {
  data: ProjectSummary[];
};
