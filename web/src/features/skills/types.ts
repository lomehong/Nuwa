/** `GET /api/v1/projects/{repo}/skills` response (generated from the Rust DTOs). */

import type { ProjectSkillsResponse as ApiProjectSkillsResponse } from "../../lib/api-types/ProjectSkillsResponse";
import type { SkillDto as ApiSkillDto } from "../../lib/api-types/SkillDto";

export type SkillSource = "builtin" | "repo";
export type SkillSelectionMode = "auto" | "pinned";

export type SkillDto = Omit<ApiSkillDto, "source"> & {
  source: SkillSource;
};

export type ProjectSkillsResponse = Omit<ApiProjectSkillsResponse, "skills" | "selection_mode"> & {
  selection_mode: SkillSelectionMode;
  skills: SkillDto[];
};
