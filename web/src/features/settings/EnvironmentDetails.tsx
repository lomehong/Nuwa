import { formatDateTime } from "../overview/format";
import type { SettingsResponse } from "./types";

/** Read-only env metadata shown beside the environment fields, not in them. */
export function EnvironmentDetails({ data }: { data: SettingsResponse }) {
  return (
    <div className="st-envdetails">
      <h3>环境状态</h3>
      <dl className="st-detail-row">
        <dt>文件状态</dt>
        <dd>{data.env_exists ? "存在" : "尚未创建（首次保存时创建）"}</dd>
      </dl>
      <dl className="st-detail-row">
        <dt>权限</dt>
        <dd>{data.env_permissions ?? "n/a"}</dd>
      </dl>
      <div className="st-detail-row">
        <dt>未识别键</dt>
        <dd>
          {data.unknown_keys.length === 0 ? (
            <span className="st-detail-empty">无</span>
          ) : (
            <ul className="st-detail-list">
              {data.unknown_keys.map((key) => (
                <li key={key}>
                  <code>{key}</code>
                </li>
              ))}
            </ul>
          )}
        </dd>
      </div>
      <div className="st-detail-row">
        <dt>最近变更</dt>
        <dd>
          {data.last_changes.length === 0 ? (
            <span className="st-detail-empty">暂无变更记录</span>
          ) : (
            <ul className="st-detail-list is-stacked">
              {data.last_changes.map((change) => (
                <li key={`${change.key}-${change.ts_ms}`}>
                  <code>{change.key}</code>{" "}
                  <span className="st-detail-empty">
                    {formatDateTime(change.ts_ms)} · {change.actor ?? "系统"}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </dd>
      </div>
    </div>
  );
}
