/** `/api/v1/benchmarks` projections (generated from the Rust DTOs). */

import type { BenchmarkDetailResponse as ApiBenchmarkDetailResponse } from "../../lib/api-types/BenchmarkDetailResponse";
import type { BenchmarkRunDto as ApiBenchmarkRunDto } from "../../lib/api-types/BenchmarkRunDto";
import type { BenchmarkRunsResponse as ApiBenchmarkRunsResponse } from "../../lib/api-types/BenchmarkRunsResponse";

export type { ScenarioMeta } from "../../lib/api-types/ScenarioMeta";
export type { BenchmarkResultDto } from "../../lib/api-types/BenchmarkResultDto";
export type { ScenariosResponse } from "../../lib/api-types/ScenariosResponse";

export type BenchmarkMode = "faux" | "live";
export type BenchmarkStatus = "running" | "completed" | "failed";

export type BenchmarkRunDto = Omit<ApiBenchmarkRunDto, "mode" | "status"> & {
  mode: BenchmarkMode;
  status: BenchmarkStatus;
};

export type BenchmarkRunsResponse = Omit<ApiBenchmarkRunsResponse, "data"> & {
  data: BenchmarkRunDto[];
};

export type BenchmarkDetailResponse = Omit<ApiBenchmarkDetailResponse, "run"> & {
  run: BenchmarkRunDto;
};
