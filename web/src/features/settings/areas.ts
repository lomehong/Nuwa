/**
 * Settings areas: one view per area (tasteskill app-screens §8.2). The env
 * schema groups are mapped onto four operator-facing areas; each area owns its
 * own save, so a view never edits fields it does not show.
 */

import type {
  LastChangeDto,
  SettingDto,
  SettingValueDto,
  SettingsResponse,
} from "./types";
import { isPlainValue, isSecretValue } from "./types";

export const SETTINGS_AREA_IDS = [
  "connection",
  "execution",
  "governance",
  "environment",
] as const;

export type SettingsAreaId = (typeof SETTINGS_AREA_IDS)[number];

export interface SettingsAreaSectionDef {
  id: string;
  title: string;
  /** Schema group for the section; keys are optional. */
  group?: SettingDto["group"];
  /** Explicit display order; fields outside every section fall into "其他". */
  keys?: readonly string[];
}

export interface SettingsAreaDef {
  id: SettingsAreaId;
  path: string;
  label: string;
  description: string;
  sectionTitle: string;
  sectionDescription: string;
  impact: string;
  groups: readonly SettingDto["group"][];
  sections: readonly SettingsAreaSectionDef[];
  related?: { label: string; to: string };
}

export const SETTINGS_AREAS: readonly SettingsAreaDef[] = [
  {
    id: "connection",
    path: "/settings/connection",
    label: "连接",
    description: "令牌、模型提供方与端点。这些值影响修复 runner。",
    sectionTitle: "连接参数",
    sectionDescription: "保存仅写文件；重启相关 work / serve 后重新上报确认，CLI 覆盖仍优先。",
    impact: "影响 runner · 保存后需重启",
    groups: ["credentials", "model"],
    sections: [
      { id: "credentials", title: "凭据", group: "credentials" },
      { id: "model", title: "模型", group: "model" },
    ],
    related: { label: "查看运行器", to: "/runners" },
  },
  {
    id: "execution",
    path: "/settings/execution",
    label: "执行",
    description: "轮询、超时、预算与 runner 身份。这些值由 work 进程读取。",
    sectionTitle: "运行参数",
    sectionDescription: "保存仅写文件；重启 work 后重新上报确认，CLI 覆盖仍优先。PR 按 UTC 准入日槽位计费，包含在途与结果不明，重试保留原日。单次 token 预算是修复交付门；每日 / 滚动 30 天预算在准入及交付前检查已记录用量，失败也计入，并发或在途消耗可能超额，并非运行时或 provider 硬上限。策略由创建进程解析，各进程可能不同。",
    impact: "影响 work · 保存后需重启",
    groups: ["runner"],
    sections: [
      {
        id: "polling",
        title: "轮询与超时",
        keys: ["NUWA_INTERVAL", "NUWA_CI_TIMEOUT"],
      },
      {
        id: "budgets",
        title: "准入预算与交付门",
        keys: [
          "NUWA_DAILY_PR_LIMIT",
          "NUWA_MAX_TURNS",
          "NUWA_MAX_TOKENS",
          "NUWA_DAILY_TOKEN_LIMIT",
          "NUWA_MONTHLY_TOKEN_LIMIT",
        ],
      },
      {
        id: "identity",
        title: "身份",
        keys: ["NUWA_RUNNER_ID", "NUWA_SERVICE_UNIT"],
      },
    ],
    related: { label: "查看运行器", to: "/runners" },
  },
  {
    id: "governance",
    path: "/settings/governance",
    label: "治理",
    description: "会话、保留窗口与事件通知。这些值由 serve 进程读取。",
    sectionTitle: "治理参数",
    sectionDescription: "保存仅写文件；重启 serve 后重新上报确认，CLI 覆盖仍优先。",
    impact: "影响 serve · 保存后需重启",
    groups: ["security", "ops"],
    sections: [
      {
        id: "session",
        title: "会话",
        keys: ["NUWA_COOKIE_SECURE", "NUWA_SESSION_TTL_SECS"],
      },
      {
        id: "events",
        title: "事件与通知",
        keys: [
          "NUWA_WEBHOOK_URL",
          "NUWA_WEBHOOK_SECRET",
          "NUWA_WEBHOOK_EVENTS",
          "NUWA_SYNC_INTERVAL",
          "NUWA_STUCK_AFTER_SECS",
          "NUWA_EVENT_RETENTION_DAYS",
        ],
      },
    ],
    related: { label: "查看服务状态", to: "/system" },
  },
  {
    id: "environment",
    path: "/settings/environment",
    label: "环境",
    description: "env 文件、服务编排与审计记录。",
    sectionTitle: "环境参数",
    sectionDescription: "只读项直接展示；可写项保存后写入 env，重启相关进程后重新上报确认。",
    impact: "本机 · 部分只读",
    groups: ["service", "advanced"],
    sections: [
      {
        id: "units",
        title: "服务编排",
        keys: ["NUWA_SERVE_UNIT", "NUWA_RUNNER_UNITS"],
      },
      { id: "env", title: "env 文件", keys: ["NUWA_ENV_FILE"] },
    ],
    related: { label: "查看服务状态", to: "/system" },
  },
];

export const DEFAULT_SETTINGS_AREA: SettingsAreaId = "connection";

const FULL_WIDTH_KEYS = new Set([
  "NUWA_MONTHLY_TOKEN_LIMIT",
  "NUWA_ENV_FILE",
]);

export function isFullWidthKey(key: string): boolean {
  return FULL_WIDTH_KEYS.has(key);
}

export function isSettingsAreaId(value: string | undefined | null): value is SettingsAreaId {
  return (
    value !== undefined &&
    value !== null &&
    (SETTINGS_AREA_IDS as readonly string[]).includes(value)
  );
}

export function settingsAreaFromPath(pathname: string): SettingsAreaDef | null {
  const match = /^\/settings\/([^/]+)\/?$/.exec(pathname);
  return match && isSettingsAreaId(match[1]) ? settingsAreaById(match[1]) : null;
}

export function settingsAreaById(id: string): SettingsAreaDef | null {
  return SETTINGS_AREAS.find((area) => area.id === id) ?? null;
}

export function areaSpecs(area: SettingsAreaDef, schema: readonly SettingDto[]): SettingDto[] {
  return schema.filter((spec) => area.groups.includes(spec.group));
}

export interface AreaSectionView {
  id: string;
  title: string;
  specs: SettingDto[];
}

/**
 * Section order comes from the area definition; a field the server added but
 * no section names still appears under "其他" instead of disappearing.
 */
export function buildAreaSections(
  area: SettingsAreaDef,
  schema: readonly SettingDto[],
): AreaSectionView[] {
  const specs = areaSpecs(area, schema);
  const used = new Set<string>();
  const sections: AreaSectionView[] = [];
  for (const definition of area.sections) {
    const order = new Map(definition.keys?.map((key, index) => [key, index]));
    const sectionSpecs = specs
      .filter((spec) => {
        if (definition.group && spec.group !== definition.group) {
          return false;
        }
        return definition.keys ? definition.keys.includes(spec.key) : true;
      })
      .sort((left, right) => {
        const leftOrder = order.get(left.key) ?? 0;
        const rightOrder = order.get(right.key) ?? 0;
        return leftOrder - rightOrder;
      });
    for (const spec of sectionSpecs) {
      used.add(spec.key);
    }
    if (sectionSpecs.length > 0) {
      sections.push({ id: definition.id, title: definition.title, specs: sectionSpecs });
    }
  }
  const rest = specs.filter((spec) => !used.has(spec.key));
  if (rest.length > 0) {
    sections.push({ id: "other", title: "其他", specs: rest });
  }
  return sections;
}

export function areaSpecByKey(
  area: SettingsAreaDef,
  schema: readonly SettingDto[],
): Map<string, SettingDto> {
  return new Map(areaSpecs(area, schema).map((spec) => [spec.key, spec]));
}

export function areaDirtySpecs(
  area: SettingsAreaDef,
  schema: readonly SettingDto[],
  drafts: Record<string, string>,
): SettingDto[] {
  return areaSpecs(area, schema).filter((spec) => drafts[spec.key] !== undefined);
}

export function currentRaw(
  spec: SettingDto,
  data: SettingsResponse,
): string {
  const value = data.values[spec.key];
  return isPlainValue(value) ? value.value : "";
}

export function effectiveRaw(
  spec: SettingDto,
  data: SettingsResponse,
  drafts: Record<string, string>,
): string {
  const draft = drafts[spec.key];
  return draft !== undefined ? draft : currentRaw(spec, data);
}

export function secretLabel(value: SettingValueDto | undefined): string {
  if (!isSecretValue(value) || !value.set) {
    return "未设置";
  }
  return value.last4 ? `已设置 · 末 4 位 ${value.last4}` : "已设置 · 末 4 位不可显示";
}

export function secretIsSet(value: SettingValueDto | undefined): boolean {
  return isSecretValue(value) && value.set;
}

export function specSource(value: SettingValueDto | undefined): string | null {
  return isPlainValue(value) && value.source === "env" ? "env 文件" : null;
}

export function latestAreaChange(
  area: SettingsAreaDef,
  schema: readonly SettingDto[],
  changes: readonly LastChangeDto[],
): LastChangeDto | null {
  const keys = new Set(areaSpecs(area, schema).map((spec) => spec.key));
  let latest: LastChangeDto | null = null;
  for (const change of changes) {
    if (keys.has(change.key) && (latest === null || change.ts_ms > latest.ts_ms)) {
      latest = change;
    }
  }
  return latest;
}

/** A compact unit for int fields; the server has no unit column. */
export function unitForSpec(spec: SettingDto): string | null {
  if (spec.type !== "int") {
    return null;
  }
  if (spec.key.endsWith("_DAYS")) {
    return "天";
  }
  if (spec.key.includes("TOKEN")) {
    return "token";
  }
  if (spec.key === "NUWA_DAILY_PR_LIMIT") {
    return "槽位";
  }
  if (spec.key === "NUWA_MAX_TURNS") {
    return "turn";
  }
  if (spec.key.includes("SECS") || spec.key.includes("INTERVAL") || spec.key.includes("TIMEOUT")) {
    return "秒";
  }
  return null;
}

export function formatSecondsShort(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) {
    return "—";
  }
  if (seconds > 0 && seconds % 86_400 === 0) {
    return `${seconds / 86_400} 天`;
  }
  if (seconds > 0 && seconds % 3_600 === 0) {
    return `${seconds / 3_600} 小时`;
  }
  if (seconds > 0 && seconds % 60 === 0) {
    return `${seconds / 60} 分钟`;
  }
  return `${seconds} 秒`;
}