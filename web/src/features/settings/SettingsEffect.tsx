import { clsx } from "clsx";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";

import { formatTokens } from "../overview/format";
import {
  currentRaw,
  effectiveRaw,
  formatSecondsShort,
  secretIsSet,
  secretLabel,
  unitForSpec,
  type SettingsAreaDef,
} from "./areas";
import { EnvironmentDetails } from "./EnvironmentDetails";
import type { SettingDto, SettingsResponse } from "./types";

interface SettingsEffectProps {
  area: SettingsAreaDef;
  data: SettingsResponse;
  specs: readonly SettingDto[];
  drafts: Record<string, string>;
  /** Connection probes; the only area with an action beyond save. */
  testActions?: ReactNode;
  lastSave?: { changed: string[]; restartRequired: boolean } | null;
}

interface EffectRow {
  label: string;
  value: string;
  changed?: boolean;
}

interface ChangeRow {
  label: string;
  old: string;
  next: string;
  secret: boolean;
}

function numeric(raw: string): number | null {
  if (raw.trim() === "") {
    return null;
  }
  const value = Number(raw);
  return Number.isFinite(value) ? value : null;
}

function runtimeValue(spec: SettingDto, raw: string): string {
  const unit = unitForSpec(spec);
  if (spec.key.includes("TOKEN")) {
    const value = numeric(raw);
    return value === null ? raw : formatTokens(value);
  }
  if (unit === "秒") {
    return `${raw}s`;
  }
  return unit ? `${raw} ${unit}` : raw;
}

function restartTarget(area: SettingsAreaDef): string {
  if (area.id === "governance") {
    return "serve";
  }
  if (area.id === "environment") {
    return "unit";
  }
  return "work";
}

/**
 * The left-hand column answers "what changes when I save": the resulting
 * behavior, the keys that will change, and the runtime values that are still in
 * effect until a restart. It carries the product-specific evidence the form
 * itself cannot show.
 */
export function SettingsEffect({
  area,
  data,
  specs,
  drafts,
  testActions,
  lastSave,
}: SettingsEffectProps) {
  const byKey = new Map(specs.map((spec) => [spec.key, spec]));
  const raw = (key: string): string => {
    const spec = byKey.get(key);
    return spec ? effectiveRaw(spec, data, drafts) : "";
  };
  const changed = (key: string): boolean => drafts[key] !== undefined;
  const has = (key: string): boolean => byKey.has(key);

  const dirtySpecs = specs.filter((spec) => drafts[spec.key] !== undefined);
  const restartDirty = dirtySpecs.filter((spec) => spec.requires_restart).length;
  const runtimeDiffs = specs.filter((spec) => data.effective_diff[spec.key]);

  const rows: EffectRow[] = [];
  if (area.id === "connection") {
    rows.push({
      label: "模型提供方",
      value: raw("NUWA_PROVIDER") || "anthropic",
      changed: changed("NUWA_PROVIDER"),
    });
    rows.push({
      label: "模型",
      value: raw("NUWA_MODEL") || "provider 默认",
      changed: changed("NUWA_MODEL"),
    });
    rows.push({
      label: "端点",
      value: raw("NUWA_BASE_URL") || "默认",
      changed: changed("NUWA_BASE_URL"),
    });
    const github = byKey.get("NUWA_GITHUB_TOKEN");
    if (github) {
      rows.push({
        label: "GitHub",
        value: changed("NUWA_GITHUB_TOKEN")
          ? "将更新"
          : secretLabel(data.values["NUWA_GITHUB_TOKEN"]),
        changed: changed("NUWA_GITHUB_TOKEN"),
      });
    }
    const unset = specs.filter(
      (spec) =>
        spec.secret &&
        !secretIsSet(data.values[spec.key]) &&
        drafts[spec.key] === undefined,
    ).length;
    if (unset > 0) {
      rows.push({ label: "待配置密钥", value: `${unset} 个` });
    }
  } else if (area.id === "execution") {
    if (has("NUWA_INTERVAL")) {
      rows.push({
        label: "下一次轮询",
        value: `${raw("NUWA_INTERVAL") || "—"}s 后`,
        changed: changed("NUWA_INTERVAL"),
      });
    }
    if (has("NUWA_CI_TIMEOUT")) {
      const seconds = numeric(raw("NUWA_CI_TIMEOUT"));
      rows.push({
        label: "CI 最长等待",
        value: seconds === null ? "—" : formatSecondsShort(seconds),
        changed: changed("NUWA_CI_TIMEOUT"),
      });
    }
    if (has("NUWA_DAILY_PR_LIMIT")) {
      rows.push({
        label: "每日 PR 上限",
        value: `${raw("NUWA_DAILY_PR_LIMIT") || "—"} 个`,
        changed: changed("NUWA_DAILY_PR_LIMIT"),
      });
    }
    if (has("NUWA_DAILY_TOKEN_LIMIT")) {
      const tokens = numeric(raw("NUWA_DAILY_TOKEN_LIMIT"));
      rows.push({
        label: "每日 token 上限",
        value: tokens === null ? "—" : formatTokens(tokens),
        changed: changed("NUWA_DAILY_TOKEN_LIMIT"),
      });
    }
  } else if (area.id === "governance") {
    if (has("NUWA_SESSION_TTL_SECS")) {
      const seconds = numeric(raw("NUWA_SESSION_TTL_SECS"));
      rows.push({
        label: "会话有效期",
        value: seconds === null ? "—" : formatSecondsShort(seconds),
        changed: changed("NUWA_SESSION_TTL_SECS"),
      });
    }
    if (has("NUWA_COOKIE_SECURE")) {
      rows.push({
        label: "Cookie secure",
        value: raw("NUWA_COOKIE_SECURE") || "auto",
        changed: changed("NUWA_COOKIE_SECURE"),
      });
    }
    if (has("NUWA_WEBHOOK_URL")) {
      rows.push({
        label: "Webhook",
        value: raw("NUWA_WEBHOOK_URL") ? "已设置" : "未设置",
        changed: changed("NUWA_WEBHOOK_URL"),
      });
    }
    if (has("NUWA_EVENT_RETENTION_DAYS")) {
      rows.push({
        label: "事件保留",
        value: `${raw("NUWA_EVENT_RETENTION_DAYS") || "—"} 天`,
        changed: changed("NUWA_EVENT_RETENTION_DAYS"),
      });
    }
  } else {
    rows.push({ label: "env 文件", value: data.env_path || "—" });
    const serve = raw("NUWA_SERVE_UNIT");
    const runners = raw("NUWA_RUNNER_UNITS");
    if (serve || runners) {
      rows.push({
        label: "服务编排",
        value: [serve, runners].filter(Boolean).join(" · "),
        changed: changed("NUWA_SERVE_UNIT") || changed("NUWA_RUNNER_UNITS"),
      });
    }
  }

  const changes: ChangeRow[] = dirtySpecs.map((spec) => ({
    label: spec.label,
    old: spec.secret ? "已设置" : effectiveRaw(spec, data, {}),
    next: spec.secret ? "将更新" : drafts[spec.key] || "空",
    secret: spec.secret,
  }));

  const envLines = dirtySpecs.map((spec) => ({
    key: spec.key,
    value: spec.secret ? "****" : drafts[spec.key] || "",
    comment: false,
  }));

  const currentEnvLines: Array<{ key: string; value: string; comment: boolean }> = [];
  for (const spec of specs) {
    if (spec.read_only) {
      continue;
    }
    if (spec.secret) {
      currentEnvLines.push(
        secretIsSet(data.values[spec.key])
          ? { key: spec.key, value: "****", comment: false }
          : { key: spec.key, value: "未设置", comment: true },
      );
    } else {
      currentEnvLines.push({
        key: spec.key,
        value: currentRaw(spec, data),
        comment: false,
      });
    }
  }

  const envPreviewLines = dirtySpecs.length > 0 ? envLines : currentEnvLines;

  const summaryText = rows
    .slice(0, 3)
    .map((row) => `${row.label} ${row.value}`)
    .join(" · ");

  const runtimeText = runtimeDiffs
    .map((spec) => `${spec.label} ${runtimeValue(spec, data.effective_diff[spec.key].runtime)}`)
    .join(" · ");
  const target = restartTarget(area);
  const pendingRestart =
    data.restart_pending || (lastSave?.restartRequired ?? false);

  return (
    <>
      <section className="st-effect" aria-label="保存后的行为">
        <p className="st-effect-head">
          <span>保存后</span>
          <span
            className={clsx("st-dot", dirtySpecs.length > 0 ? "warn" : "ok")}
            aria-hidden="true"
          />
        </p>
        <dl className="st-effect-rows">
          {rows.map((row) => (
            <div className="st-effect-row" key={row.label}>
              <dt>{row.label}</dt>
              <dd className={clsx(row.changed && "is-changed")}>{row.value}</dd>
            </div>
          ))}
        </dl>

        {dirtySpecs.length > 0 ? (
          <>
            <div className="st-effect-sep" />
            <p className="st-effect-head">
              <span>将改变</span>
              <span>
                {dirtySpecs.length} 项
                {restartDirty > 0 ? ` · ${restartDirty} 项需重启` : ""}
              </span>
            </p>
            <dl className="st-effect-rows">
              {changes.map((change) => (
                <div className="st-effect-row" key={change.label}>
                  <dt>{change.label}</dt>
                  <dd>
                    {!change.secret ? <span className="old">{change.old || "空"}</span> : null}
                    <span className="new">{change.next}</span>
                  </dd>
                </div>
              ))}
            </dl>
          </>
        ) : null}

        <div className="st-effect-sep" />
        <div className="st-envhead">
          <span>写入 nuwa.env</span>
          <code>{dirtySpecs.length > 0 ? "仅本区键" : "当前值"}</code>
        </div>
        {envPreviewLines.length > 0 ? (
          <pre className="st-env">
            {envPreviewLines.map((line) => (
              <span key={line.key}>
                {line.comment ? (
                  <span className="comment"># {line.key} 未设置</span>
                ) : (
                  <>
                    <span className="k">{line.key}</span>=
                    <span className={clsx("v", dirtySpecs.length > 0 && "changed")}>
                      {line.value}
                    </span>
                  </>
                )}
                {"\n"}
              </span>
            ))}
          </pre>
        ) : (
          <p className="st-envnote">本区没有可写的 env 键。</p>
        )}
        <p className="st-envnote">
          {dirtySpecs.length > 0
            ? `其余 ${specs.length - dirtySpecs.length} 个键保持不变。`
            : "保存后 env 文件不变。"}
        </p>
      </section>

      <p className="st-mobile-summary">保存后：{summaryText}</p>

      {area.id === "environment" ? <EnvironmentDetails data={data} /> : null}

      {runtimeDiffs.length > 0 ? (
        <p className="st-runtime">
          <span className="st-dot warn" aria-hidden="true" />
          <span>
            运行中仍是 <b>{runtimeText}</b>；保存并重启 {target} 后切换。
          </span>
        </p>
      ) : null}

      {pendingRestart ? (
        <p className="st-runtime">
          <span className="st-dot warn" aria-hidden="true" />
          <span>
            {lastSave?.restartRequired
              ? `已保存；重启 ${target} 后生效。`
              : `有配置等待重启；重启 ${target} 后生效。`}
            <Link to="/system">去服务页重启 →</Link>
          </span>
        </p>
      ) : null}

      {testActions ? (
        <div className="st-test">
          <p className="st-test-title">连接测试</p>
          {testActions}
        </div>
      ) : null}

      {area.related ? (
        <Link className="st-related" to={area.related.to}>
          {area.related.label} →
        </Link>
      ) : null}
    </>
  );
}
