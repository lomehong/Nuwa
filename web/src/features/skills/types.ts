/** `GET /api/v1/projects/{repo}/skills` response. */

export type SkillSource = "builtin" | "repo";
export type SkillSelectionMode = "auto" | "pinned";

export interface SkillDto {
  name: string;
  version: number;
  description: string;
  languages: string[];
  matches: string[];
  body: string;
  body_truncated: boolean;
  source: SkillSource;
  /** `builtin:<file>` or the repo-relative `.nuwa/skills/<file>` path. */
  origin: string;
  /** False when a repo skill shadows this built-in definition. */
  effective: boolean;
  overrides_builtin: boolean;
  allowlisted: boolean;
}

export interface ProjectSkillsResponse {
  repo: string;
  selection_mode: SkillSelectionMode;
  allowlist: string[];
  repo_path: string | null;
  skills: SkillDto[];
  warnings: string[];
}
