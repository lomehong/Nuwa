import { Power, RefreshCw } from "lucide-react";
import { useState, type ReactNode } from "react";

import { Skeleton } from "../../components/ui";
import { describeError } from "../../lib/errors";
import { formatDuration } from "../overview/format";
import { PanelError } from "../overview/PanelChrome";
import { SyncButton } from "../actions/SyncButton";
import { RestartDialog } from "./RestartDialog";
import { useSystemQuery } from "./queries";

const REPO_URL = "https://github.com/lomehong/nuwa";

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${value.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

function DetailRow({
  label,
  children,
  mono = true,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
}) {
  return (
    <dl className="st-detail-row">
      <dt>{label}</dt>
      <dd className={mono ? undefined : "st-detail-empty"}>{children}</dd>
    </dl>
  );
}

function SystemSkeleton() {
  return (
    <div className="st-screen">
      <div className="st-content" role="status" aria-busy="true">
        <span className="sr-only">正在加载服务状态</span>
        <div className="st-head">
          <Skeleton className="h-6 w-48" />
        </div>
        <Skeleton className="mt-6 h-40 w-full rounded-modal" />
        <Skeleton className="mt-4 h-40 w-full rounded-modal" />
      </div>
    </div>
  );
}

/**
 * Service page: runtime status, systemd units, the two process-level actions
 * (GitHub reconcile and restart), and about. Settings themselves live in the
 * four /settings views; this page never edits env.
 */
export function SystemPage() {
  const system = useSystemQuery();
  const [restartOpen, setRestartOpen] = useState(false);
  const data = system.data;

  if (system.isPending && !data) {
    return <SystemSkeleton />;
  }
  if (!data) {
    return (
      <div className="st-screen">
        <div className="st-content">
          <PanelError
            title="无法加载服务状态"
            error={system.error}
            onRetry={() => void system.refetch()}
          />
        </div>
      </div>
    );
  }

  const uptime = formatDuration(data.uptime_secs * 1_000);
  const unitLabel = data.services.length > 0 ? data.services.map((unit) => unit.unit).join("、") : "未配置 systemd unit";

  return (
    <div className="st-screen">
      <div className="st-content">
        <div className="st-head">
          <div>
            <p className="st-desc">serve 进程、systemd units 与数据库状态。</p>
            <p className="st-finding">
              <span className="st-dot ok" aria-hidden="true" />
              v{data.version} · 已运行 {uptime}
            </p>
          </div>
          <div className="st-head-right">
            <button
              className="st-btn"
              type="button"
              aria-busy={system.isFetching || undefined}
              onClick={() => void system.refetch()}
            >
              <RefreshCw aria-hidden="true" className={system.isFetching ? "size-3.5 animate-spin" : "size-3.5"} />
              刷新
            </button>
          </div>
        </div>

        {system.isError ? (
          <div className="st-notices" role="status">
            <p className="st-notice">
              <span className="st-dot warn" aria-hidden="true" />
              状态更新失败，显示上次数据。{describeError(system.error)}
            </p>
          </div>
        ) : null}

        {data.local_mode ? (
          <div className="st-notices" role="status">
            <p className="st-notice">
              <span className="st-dot warn" aria-hidden="true" />
              本地模式 · API 无鉴权：任何能访问该端口的客户端都可修改设置或重启服务。
            </p>
          </div>
        ) : null}

        <div className="st-section">
          <div className="st-label">
            <h2>运行状态</h2>
            <p className="st-label-desc">进程版本、数据库与鉴权模式。</p>
          </div>
          <section className="st-panel" aria-label="运行状态">
            <DetailRow label="版本">{data.version || "—"}</DetailRow>
            <DetailRow label="构建">{data.build_commit || "dev"}</DetailRow>
            <DetailRow label="运行时长">{uptime}</DetailRow>
            <DetailRow label="数据库路径">{data.db_path || "—"}</DetailRow>
            <DetailRow label="数据库大小">{formatBytes(data.db_size_bytes)}</DetailRow>
            <DetailRow label="鉴权模式" mono={false}>
              {data.local_mode ? "本地模式" : "令牌 / 会话"}
            </DetailRow>
          </section>
        </div>

        <div className="st-section">
          <div className="st-label">
            <h2>服务单元</h2>
            <p className="st-label-desc">重启时由 systemd 编排的 unit；同一 unit 承载的多个 runner 会一起重启。</p>
          </div>
          <section className="st-panel" aria-label="服务单元">
            {data.services.length === 0 ? (
              <p className="st-help">未配置 systemd unit。</p>
            ) : (
              data.services.map((service) => (
                <DetailRow key={service.unit} label={service.unit}>
                  <span
                    className={
                      service.active === "active"
                        ? "st-dot ok"
                        : service.active === "n/a"
                          ? "st-dot off"
                          : "st-dot warn"
                    }
                    aria-hidden="true"
                  />
                  {service.active}
                </DetailRow>
              ))
            )}
          </section>
        </div>

        <div className="st-section">
          <div className="st-label">
            <h2>关于</h2>
            <p className="st-label-desc">版本、许可证与源码位置。</p>
          </div>
          <section className="st-panel" aria-label="关于">
            <DetailRow label="版本">{data.version || "—"}</DetailRow>
            <DetailRow label="许可证" mono={false}>
              MIT
            </DetailRow>
            <DetailRow label="源码" mono={false}>
              <a
                href={REPO_URL}
                target="_blank"
                rel="noreferrer"
                className="text-accent underline-offset-2 hover:underline"
              >
                {REPO_URL}
              </a>
            </DetailRow>
          </section>
        </div>

        <div className="st-section">
          <div className="st-label">
            <h2>维护</h2>
            <p className="st-label-desc">
              GitHub 对账会更新 PR 状态；重启会中断正在进行的修复，并等待服务恢复。
            </p>
          </div>
          <section className="st-panel" aria-label="维护">
            <div className="flex flex-wrap items-center gap-3">
              <SyncButton />
              <button
                className="st-btn danger"
                type="button"
                onClick={() => setRestartOpen(true)}
              >
                <Power aria-hidden="true" className="size-3.5" />
                重启服务
              </button>
            </div>
            <p className="st-help">将重启：{unitLabel}。</p>
          </section>
        </div>
      </div>

      <RestartDialog
        open={restartOpen}
        onClose={() => setRestartOpen(false)}
        units={data.services.map((service) => service.unit)}
      />
    </div>
  );
}
