/**
 * Wire shapes for the settings and system endpoints (docs/09 §9.2 settings,
 * §9.2 system, §13). Response fields come from the Rust DTOs via ts-rs; the
 * enum-ish strings (`group`, `type`, `scope`) are narrowed to the UI unions.
 * Request shapes stay hand-written: the client sends a narrower scalar map
 * than the server's `serde_json::Value`.
 */

import type { DiffDto } from "../../lib/api-types/DiffDto";
import type { LastChangeDto } from "../../lib/api-types/LastChangeDto";
import type { SettingDto as ApiSettingDto } from "../../lib/api-types/SettingDto";
import type { SettingsResponse as ApiSettingsResponse } from "../../lib/api-types/SettingsResponse";

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
export type SettingDto = Omit<ApiSettingDto, "group" | "type" | "scope"> & {
  group: SettingGroup;
  type: SettingType;
  scope: SettingScope;
};

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

export type { DiffDto, LastChangeDto };

export type SettingsResponse = Omit<
  ApiSettingsResponse,
  "schema" | "values" | "effective_diff" | "last_changes"
> & {
  schema: SettingDto[];
  values: Record<string, SettingValueDto>;
  effective_diff: Record<string, DiffDto>;
  last_changes: LastChangeDto[];
};

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
