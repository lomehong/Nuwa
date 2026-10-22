/**
 * Wire shapes for `GET /api/v1/runners` (`crates/nuwa/src/api/runners.rs`).
 * Field names and nullability mirror the serializer exactly.
 */

export interface EffectiveConfig {
  interval_secs: number | null;
  ci_timeout_secs: number | null;
  max_turns: number | null;
  max_tokens: number | null;
  failing_check: string | null;
  reproduce_check?: string | null;
  forbidden_paths_count: number | null;
  max_attempts?: number | null;
  draft_first_n?: number | null;
  skills?: string[];
}

export interface RunnerDto {
  id: string;
  repo: string;
  host: string;
  pid: number | null;
  started_at_ms: number;
  heartbeat_at_ms: number;
  progress_at_ms?: number;
  status: string;
  offline: boolean;
  current_fingerprint: string | null;
  phase: string | null;
  provider: string | null;
  model: string | null;
  daily_pr_limit: number | null;
  prs_today: number;
  repo_path: string | null;
  service_unit: string | null;
  last_error: string | null;
  effective: EffectiveConfig;
}

export interface RunnerListResponse {
  data: RunnerDto[];
}
