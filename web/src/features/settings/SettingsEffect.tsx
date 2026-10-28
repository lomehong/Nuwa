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
  type SettingsAreaDef,
} from "./areas";
import { EnvironmentDetails } from "./EnvironmentDetails";
import { ProcessConfiguration } from "./ProcessConfiguration";
import type { SettingDto, SettingsResponse } from "./types";

interface SettingsEffectProps {
  area: SettingsAreaDef;
  data: SettingsResponse;
  specs: readonly SettingDto[];
  drafts: Record<string, string>;
  /** Connection probes; the only area with an action beyond save. */
  testActions?: ReactNode;
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

/**
 * Saved/draft candidates are not runtime evidence. Each relevant process
 * reports its own startup revision, actual values and independent freshness.
 */
export function SettingsEffect({
  area,
  data,
  specs,
  drafts,
  testActions,
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
  const processes = data.processes.filter((process) =>
    !["serve", "work"].includes(process.role) ||
    specs.some((spec) => spec.scope === "both" || spec.scope === process.role),
  );

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
        label: "候选轮询间隔",
        value: `${raw("NUWA_INTERVAL") || "—"}s`,
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

  const candidateLabel = dirtySpecs.length > 0 ? "草稿候选" : "已保存候选";
  const savedLabel = data.saved.state === "saved" && data.saved.revision
    ? "已保存（saved）"
    : data.saved.state === "unversioned"
      ? "未建立版本（unversioned）"
      : "未知（unknown），不能确认保存版本";

  return (
    <>
      <section className="st-effect" aria-label="候选配置预览">
        <p className="st-effect-head">
          <span>{candidateLabel}</span>
          <span
            className={clsx("st-dot", dirtySpecs.length > 0 ? "warn" : "ok")}
            aria-hidden="true"
          />
        </p>
        <p className="st-envnote">仅为候选预览；保存不会改变运行进程。需重启相关进程，并以其启动上报确认生效；CLI 覆盖不会因重启而消失。</p>
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
          <code>{dirtySpecs.length > 0 ? "仅本区草稿键" : "已保存文件 / 默认值"}</code>
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

      <p className="st-mobile-summary">{candidateLabel}（需重启相关进程）：{summaryText}</p>

      {area.id === "environment" ? <EnvironmentDetails data={data} /> : null}

      <section className="my-4 space-y-2 break-all text-caption" aria-label="保存状态">
        <h3 className="font-semibold">保存状态：{savedLabel}</h3>
        <p>文件身份：<code>{data.saved.env_id || "未知"}</code></p>
        <p>保存版本：<code>{data.saved.revision ?? "未知"}</code></p>
        {data.saved.reason ? <p>原因：{data.saved.reason}</p> : null}
      </section>
      <section className="my-4 space-y-3" aria-label="本区进程配置">
        <h3 className="text-caption font-semibold">相关进程启动证据</h3>
        <p className="text-caption text-text-muted">配置状态按整个启动版本判定；实际值仅列本区适用的非密钥字段。在线不代表已加载保存版本。</p>
        {processes.length === 0 ? <p className="text-caption">没有相关进程上报；生效状态未知。</p> : processes.map((process, index) => (
          <ProcessConfiguration
            key={process.snapshot?.process_instance_id ?? `${process.role}-${process.runner_id}-${index}`}
            process={process}
            specs={specs}
          />
        ))}
      </section>
      {processes.some((process) => process.activation === "stale") ? (
        <p className="st-runtime">
          <span className="st-dot warn" aria-hidden="true" />
          <span>
            存在旧版本进程，需分别重启并重新上报确认。
            <Link to="/system">去服务页重启 →</Link>
          </span>
        </p>
      ) : null}

      {testActions ? (
        <div className="st-test">
          <p className="st-test-title">候选连接测试</p>
          <p className="st-envnote">测试已保存文件，或文件叠加本次草稿；不测试运行中的进程，也不证明已生效。</p>
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
