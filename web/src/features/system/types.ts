/**
 * Wire shapes for the service/system endpoints. The settings API still owns
 * `/settings/restart`; the system page is its only caller.
 */

export type { ServiceState } from "../../lib/api-types/ServiceState";
export type { SystemResponse } from "../../lib/api-types/SystemResponse";

export interface RestartServicesRequest {
  force?: boolean;
}

/** A guarded restart (409) lists the runs that would be interrupted. */
export interface ActiveRunSummary {
  id: string;
  fingerprint: string;
  runner_id: string;
  repo: string;
  phase: string;
  started_at_ms: number;
}

export interface RestartConflictBody {
  error: {
    code: string;
    message: string;
    fields?: Record<string, string>;
  };
  active_runs: ActiveRunSummary[];
}
