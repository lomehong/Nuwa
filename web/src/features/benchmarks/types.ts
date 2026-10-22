/** `/api/v1/benchmarks` projections. */

export interface ScenarioMeta {
  name: string;
  title: string;
  description: string;
  language: string;
  failing_check: string;
  expect_fixed: boolean;
}

export type BenchmarkMode = "faux" | "live";
export type BenchmarkStatus = "running" | "completed" | "failed";

export interface BenchmarkRunDto {
  id: string;
  started_at_ms: number;
  finished_at_ms: number | null;
  provider: string;
  model: string | null;
  mode: BenchmarkMode;
  status: BenchmarkStatus;
  scenarios: number;
  repaired: number;
  claimed: number;
  false_positive: number;
  delivery_ok: number;
  turns: number;
  tokens_uncached: number;
  tokens_cached: number;
  duration_ms: number;
  error: string | null;
}

export interface BenchmarkRunsResponse {
  data: BenchmarkRunDto[];
  scenarios: ScenarioMeta[];
}

export interface BenchmarkResultDto {
  scenario: string;
  language: string;
  expected_fixed: boolean;
  repaired: boolean;
  claimed_fixed: boolean;
  delivery_ok: boolean;
  matches_expectation: boolean;
  turns: number;
  tokens_uncached: number;
  tokens_cached: number;
  duration_ms: number;
  root_cause: string | null;
  final_report: string | null;
  error: string | null;
}

export interface BenchmarkDetailResponse {
  run: BenchmarkRunDto;
  results: BenchmarkResultDto[];
}
