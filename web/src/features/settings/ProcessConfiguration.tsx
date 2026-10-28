import type { ProcessConfigDto } from "../../lib/api-types/ProcessConfigDto";
import { formatDateTime } from "../overview/format";
import type { SettingDto } from "./types";

const SOURCES: Record<string, string> = {
  file: "文件（file）",
  environment: "环境变量（environment）",
  default: "默认值（default）",
  cli: "命令行（cli）",
  derived: "推导值（derived）",
  runtime: "运行时（runtime）",
  override: "覆盖值（override）",
};

/** Startup evidence and reporting freshness are deliberately independent. */
export function ProcessConfiguration({
  process,
  specs,
}: {
  process: ProcessConfigDto;
  /** Settings views show only non-secret keys applicable to this area/role. */
  specs?: readonly SettingDto[];
}) {
  const snapshot = process.snapshot;
  const role = process.role === "serve" || process.role === "work" ? process.role : "未知角色";
  const identity = `${role}${process.runner_id ? ` / ${process.runner_id}` : ""}`;
  const activation = process.activation === "active"
    ? "已加载保存版本（active）"
    : process.activation === "stale"
      ? "旧版本（stale）"
      : "未知（unknown），不能确认已生效";
  const freshness = process.freshness === "online"
    ? "在线（online）"
    : process.freshness === "offline"
      ? "离线（offline）"
      : process.freshness === "unreported"
        ? "未上报（unreported）"
        : "未知（unknown）";
  const keys = specs
    ? specs.filter((spec) => !spec.secret && (spec.scope === "both" || spec.scope === process.role))
      .map((spec) => spec.key)
    : Object.keys(snapshot?.values ?? {});
  const hasCli = keys.some((key) => snapshot?.values[key]?.source === "cli");

  return (
    <section className="space-y-2 rounded-card border border-border p-3 text-caption" aria-label={`${identity} 配置`}>
      <h3 className="break-all font-mono font-semibold text-text">{identity}</h3>
      <p>配置状态：{activation}</p>
      <p>上报状态：{freshness}</p>
      {process.reason ? <p className="break-all text-text-muted">原因：{process.reason}</p> : null}
      {process.freshness === "offline" ? <p>离线进程仅有历史启动证据，不代表当前仍在运行。</p> : null}
      <dl className="space-y-1 break-all text-text-secondary">
        <div><dt>进程 UUID</dt><dd className="font-mono">{snapshot?.process_instance_id ?? "未知"}</dd></div>
        {process.role === "work" ? <div><dt>Runner 实例</dt><dd className="font-mono">{snapshot?.runner_instance_id ?? "未知"}</dd></div> : null}
        <div><dt>启动时间</dt><dd>{snapshot ? formatDateTime(snapshot.started_at_ms) : "未知"}</dd></div>
        <div><dt>已加载版本</dt><dd className="font-mono">{snapshot?.loaded_revision ?? "未知"}</dd></div>
        <div><dt>配置文件身份</dt><dd className="font-mono">{snapshot?.env_id || "未知"}</dd></div>
        <div><dt>最近上报</dt><dd>{process.last_seen_at_ms === null ? "未上报" : formatDateTime(process.last_seen_at_ms)}</dd></div>
      </dl>
      {snapshot?.revision_reason ? <p className="break-all text-text-muted">启动版本未获证明：{snapshot.revision_reason}</p> : null}
      {hasCli ? <p>CLI 覆盖优先；使用相同命令重启仍保留 CLI 覆盖。要采用文件值，需修改启动命令。</p> : null}
      {!snapshot ? <p>缺少启动快照；实际值未知，不能用旧遥测推断已生效。</p> : (
        <details>
          <summary className="cursor-pointer">启动时实际配置与来源</summary>
          <dl className="mt-2 space-y-2 break-all">
            {keys.map((key) => {
              const effective = snapshot.values[key];
              return (
                <div key={key}>
                  <dt className="font-mono">{key}</dt>
                  <dd>
                    {effective?.value === null || effective?.value === undefined ? "未知 / 未提供" : effective.value || "空值"}
                    {" · 来源："}{SOURCES[effective?.source ?? ""] ?? "未知（unknown）"}
                    {effective?.reason ? ` · ${effective.reason}` : ""}
                  </dd>
                </div>
              );
            })}
          </dl>
          <p className="mt-2 text-text-muted">密钥值不包含在进程快照中。</p>
        </details>
      )}
    </section>
  );
}