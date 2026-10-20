/**
 * Wire shapes for the settings and system endpoints (docs/09 §9.2 settings,
 * §9.2 system, §13). Field names and nullability mirror the serde output of
 * `crates/nuwa/src/api/settings.rs` and `crates/nuwa/src/api/system.rs`.
 */

export type SettingGroup =
  | "credentials"
  | "model"
  | "runner"
  | "service"
  | "security"
  | "ops"
  | "advanced";

export type SettingType = "secret" | "string" | "int" | "enum" | "url" | "csv" | "path";

export type SettingScope = "both" | "work" | "serve";

/** One schema row. `type` is the serialized `kind`; `min`/`max` bound `int`. */
export interface SettingDto {
  key: string;
  label: string;
  group: SettingGroup;
  type: SettingType;
  default: string | null;
  options: string[];
  secret: boolean;
  scope: SettingScope;
  requires_restart: boolean;
  read_only: boolean;
  help: string;
  min: number | null;
  max: number | null;
}

/** Secrets never echo their value: presence, last four characters and time. */
export interface SecretValueDto {
  set: boolean;
  last4: string | null;
  updated_at_ms: number;
}

export interface PlainValueDto {
  value: string;
  source: "env" | "default" | string;
}

/** The untagged `SettingValueDto`; discriminate with {@link isSecretValue}. */
export type SettingValueDto = SecretValueDto | PlainValueDto;

export function isSecretValue(value: SettingValueDto | undefined): value is SecretValueDto {
  return value !== undefined && "set" in value;
}

export function isPlainValue(value: SettingValueDto | undefined): value is PlainValueDto {
  return value !== undefined && "value" in value;
}

/** A runner-reported runtime value that differs from the env file. */
export interface DiffDto {
  file: string;
  runtime: string;
  source: "file" | "default" | string;
}

export interface LastChangeDto {
  key: string;
  ts_ms: number;
  actor: string | null;
}

export interface SettingsResponse {
  schema: SettingDto[];
  values: Record<string, SettingValueDto>;
  env_path: string;
  env_exists: boolean;
  env_permissions: string | null;
  needs_migration: boolean;
  restart_pending: boolean;
  effective_diff: Record<string, DiffDto>;
  unknown_keys: string[];
  last_changes: LastChangeDto[];
}

export interface ServiceState {
  unit: string;
  /** `systemctl is-active` output; `n/a` when systemd is unavailable. */
  active: string;
}

export interface SystemResponse {
  version: string;
  build_commit: string;
  started_at_ms: number;
  uptime_secs: number;
  db_path: string;
  db_size_bytes: number;
  local_mode: boolean;
  services: ServiceState[];
}

/** Env scalars; an int field may be sent as a number, everything else text. */
export type SettingsValues = Record<string, string | number | boolean>;

export interface SaveSettingsRequest {
  values: SettingsValues;
}

export interface SaveSettingsResponse {
  changed: string[];
  restart_required: boolean;
}

export type TestTarget = "github" | "llm";

export interface TestSettingsRequest {
  target: TestTarget;
  /** Unsaved candidates; omitted means "use the current environment". */
  values?: SettingsValues;
}

export interface TestSettingsResponse {
  ok: boolean;
  detail: string;
}

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

