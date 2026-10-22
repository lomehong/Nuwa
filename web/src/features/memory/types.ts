/** `GET /api/v1/projects/{repo}/memory` response. */

export interface MemoryDto {
  fingerprint: string;
  title: string;
  state: string;
  commit_sha: string;
  pr_url: string | null;
  root_cause: string;
  change_stat: string;
  files: number;
  recorded_at_ms: number;
  disabled: boolean;
}

export interface MemoryListResponse {
  data: MemoryDto[];
  total: number;
  limit: number;
  offset: number;
}
