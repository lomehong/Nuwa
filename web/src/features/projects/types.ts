/**
 * Project (repository under repair) shapes for `GET /api/v1/projects`.
 *
 * A project is the console's primary object: one row per repository with the
 * numbers an owner triages on. Fields mirror the Rust serializer exactly.
 */

import type { Severity } from "../../components/ui";

export interface ProjectSummary {
  /** Repository identity — the project's stable key. */
  repo: string;
  /** Display name from the console's repository registry. */
  name: string | null;
  url: string | null;
  description: string | null;
  /** Registered in the console's repository registry. */
  registered: boolean;
  /** Open issue counts by lifecycle state. */
  states: Record<string, number>;
  /** Needs a human: escalated, rejected, stuck, or a CI timeout. */
  attention: number;
  /** Highest severity among the project's open issues — the stone mark. */
  worst_severity: Severity | null;
  queued: number;
  running: number;
  pr_open: number;
  escalated: number;
  ignored: number;
  merged: number;
  rejected: number;
  runners: number;
  runners_online: number;
  prs_today: number;
  /** Daily PR quota shared by the project's runners. */
  pr_limit: number;
  ci_green: number;
  ci_red: number;
  reports_24h: number;
  tokens_24h: number;
  last_signal_ms: number;
  last_delivery_ms: number | null;
  last_pr_url: string | null;
}

export interface ProjectsResponse {
  data: ProjectSummary[];
}
