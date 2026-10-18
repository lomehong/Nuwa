import { clsx } from "clsx";
import {
  ChevronRight,
  FlaskConical,
  GitBranch,
  Pencil,
  Plus,
  Power,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";

import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  ConfirmDialog,
  EmptyState,
  Skeleton,
  useToast,
} from "../../components/ui";
import { ApiError } from "../../lib/api";
import { describeError } from "../../lib/errors";
import { SyncButton } from "../actions/SyncButton";
import { issueDetailPath } from "../issues/format";
import { formatDateTime, formatDuration, isoDateTime, shortFingerprint } from "../overview/format";
import { PanelError } from "../overview/PanelChrome";
import { runnerIsOffline } from "../runners/format";
import { useRunnersQuery } from "../runners/queries";
import type { RunnerDto } from "../runners/types";
import { RepoFormDialog, type RepoFormPrefill } from "./RepoFormDialog";
import { RestartDialog } from "./RestartDialog";
import { SettingField } from "./SettingField";
import {
  useDeleteRepoMutation,
  useReposQuery,
  useSaveSettingsMutation,
  useSettingsQuery,
  useSystemQuery,
  useTestSettingsMutation,
} from "./queries";
import {
  isPlainValue,
  type DiffDto,
  type RepoDto,
  type SettingDto,
  type SettingGroup,
  type SettingsResponse,
  type SettingsValues,
  type SettingValueDto,
  type SystemResponse,
} from "./types";
import { validateSettingValue } from "./validation";

interface SectionDef {
  group: SettingGroup;
  title: string;
  description: string;
}

/** docs/09 §10.8: the credential/model/runner/service/security/ops/advanced order. */
const SECTIONS: readonly SectionDef[] = [
  {
    group: "credentials",
    title: "凭据",
    description: "令牌只显示是否已设置与末 4 位，保存后需重启进程才会加载。",
  },
  {
    group: "model",
    title: "模型",
    description: "修复 runner 使用的模型提供方、模型名与兼容端点。",
  },
  {
    group: "runner",
    title: "Runner",
    description: "轮询、CI 等待、turn 与 token 预算，以及 runner 身份。",
  },
  {
    group: "service",
    title: "服务控制",
    description: "systemd unit、进程版本与数据库状态。",
  },
  {
    group: "security",
    title: "安全",
    description: "会话 cookie 与有效期策略。",
  },
  {
    group: "ops",
    title: "运维",
    description: "卡单判定与事件保留窗口。",
  },
  {
    group: "advanced",
    title: "高级",
    description: "env 文件位置、权限与审计记录。",
  },
];

/** Settings sections plus the read-only repository and about cards. */
type SectionId = SettingGroup | "repo" | "about";

const NAV_ITEMS: readonly { id: SectionId; label: string }[] = [
  { id: "credentials", label: "凭据" },
  { id: "model", label: "模型" },
  { id: "runner", label: "Runner" },
  { id: "repo", label: "仓库" },
  { id: "service", label: "服务控制" },
  { id: "security", label: "安全" },
  { id: "ops", label: "运维" },
  { id: "advanced", label: "高级" },
  { id: "about", label: "关于" },
];

/** Long sections start collapsed so the page opens at a readable length. */
const INITIAL_OPEN: Record<SectionId, boolean> = {
  credentials: true,
  model: true,
  runner: true,
  repo: true,
  service: true,
  security: false,
  ops: false,
  advanced: false,
  about: false,
};

function isSchemaGroup(id: SectionId): id is SettingGroup {
  return id !== "repo" && id !== "about";
}

function sectionElementId(id: SectionId): string {
  return `settings-${id}`;
}

function panelElementId(id: SectionId): string {
  return `settings-${id}-panel`;
}

/** Unique, non-empty values in encounter order; `null`/blank entries drop out. */
function distinctStrings(values: (string | null | undefined)[]): string[] {
  const seen = new Set<string>();
  for (const value of values) {
    if (value) {
      seen.add(value);
    }
  }
  return [...seen];
}

interface RepoGroup {
  repo: string;
  runners: RunnerDto[];
}

/** A heartbeat without a repo cannot be registered; it stays read-only. */
const UNREPORTED_REPO = "（未上报仓库）";

/** Runners report a repo each heartbeat; the repository view groups by it. */
function groupRunnersByRepo(runners: RunnerDto[]): RepoGroup[] {
  const groups = new Map<string, RunnerDto[]>();
  for (const runner of runners) {
    const repo = runner.repo || UNREPORTED_REPO;
    const bucket = groups.get(repo);
    if (bucket) {
      bucket.push(runner);
    } else {
      groups.set(repo, [runner]);
    }
  }
  return [...groups].map(([repo, members]) => ({ repo, runners: members }));
}

interface RepoEntry extends RepoGroup {
  /** `null` means a runner reported an identity nobody registered. */
  registration: RepoDto | null;
}

/**
 * Registry rows and runner groups keyed by the same identity. Registry order
 * (name, from the API) wins for registered entries; runner-only groups keep
 * heartbeat order behind them.
 */
function mergeRepoEntries(registrations: RepoDto[], runners: RunnerDto[]): RepoEntry[] {
  const entries = new Map<string, RepoEntry>();
  for (const row of registrations) {
    entries.set(row.repo, { repo: row.repo, registration: row, runners: [] });
  }
  for (const group of groupRunnersByRepo(runners)) {
    const existing = entries.get(group.repo);
    if (existing) {
      existing.runners = group.runners;
    } else {
      entries.set(group.repo, {
        repo: group.repo,
        registration: null,
        runners: group.runners,
      });
    }
  }
  return [...entries.values()];
}

const REPO_URL = "https://github.com/lomehong/nuwa";

/** Keys the LLM probe sends as unsaved candidates when they hold a draft. */
const LLM_PROBE_KEYS = [
  "NUWA_PROVIDER",
  "NUWA_MODEL",
  "NUWA_BASE_URL",
  "ZAI_API_KEY",
  "ANTHROPIC_API_KEY",
] as const;

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

/** A draft is unchanged when it matches the current plain value; an empty
 * secret draft means "keep the stored value" and is never a draft at all. */
function draftIsUnchanged(
  spec: SettingDto,
  value: SettingValueDto | undefined,
  raw: string,
): boolean {
  if (spec.secret) {
    return raw === "";
  }
  if (!value || !isPlainValue(value)) {
    return raw === "";
  }
  if (spec.type === "int" && raw !== "" && value.value !== "") {
    return Number(raw) === Number(value.value);
  }
  return raw === value.value;
}

/** `/settings/` and `/settings` are the same route; trailing slashes alone must
 * not open the unsaved-changes guard. */
function normalizePathname(pathname: string): string {
  const trimmed = pathname.replace(/\/+$/, "");
  return trimmed === "" ? "/" : trimmed;
}

/** Query order is not a navigation (`?a=1&b=2` equals `?b=2&a=1`). */
function normalizeSearch(search: string): string {
  const params = new URLSearchParams(search);
  params.sort();
  return params.toString();
}

function SettingsSkeleton() {
  return (
    <div role="status" aria-busy="true" className="space-y-4">
      <span className="sr-only">正在加载设置</span>
      <Skeleton className="h-8 w-40" />
      {Array.from({ length: 3 }, (_, index) => (
        <Skeleton key={index} className="h-44 w-full" />
      ))}
    </div>
  );
}

interface SectionProps {
  id: SectionId;
  title: string;
  description: string;
  open: boolean;
  onToggle: () => void;
  /** Schema rows; omit for custom read-only panels instead of an empty hint. */
  specs?: SettingDto[];
  values: Record<string, SettingValueDto>;
  diffs: Record<string, DiffDto>;
  drafts: Record<string, string>;
  errors: Record<string, string>;
  onDraftChange: (spec: SettingDto, raw: string) => void;
  /** Field layout on wide screens; long groups pair up to stay scannable. */
  columns?: "one" | "two";
  aside?: ReactNode;
  children?: ReactNode;
}

function SectionHeader({
  title,
  description,
  open,
  controls,
  onToggle,
  aside,
}: {
  title: string;
  description: string;
  open: boolean;
  controls: string;
  onToggle: () => void;
  aside?: ReactNode;
}) {
  return (
    <CardHeader>
      <div className="min-w-0 flex-1">
        <CardTitle as="h2">
          <button
            type="button"
            aria-expanded={open}
            aria-controls={controls}
            onClick={onToggle}
            className="flex min-h-11 w-full items-center gap-2 rounded-card text-left text-heading-sm font-semibold text-text"
          >
            <ChevronRight
              aria-hidden="true"
              className={clsx(
                "size-4 shrink-0 text-text-muted transition-transform duration-150 ease-out",
                open && "rotate-90",
              )}
            />
            <span className="min-w-0">{title}</span>
          </button>
        </CardTitle>
        <CardDescription className="pl-6">{description}</CardDescription>
      </div>
      {aside ? <div className="flex flex-wrap items-center gap-2">{aside}</div> : null}
    </CardHeader>
  );
}

function SettingsSection({
  id,
  title,
  description,
  open,
  onToggle,
  specs,
  values,
  diffs,
  drafts,
  errors,
  onDraftChange,
  columns = "one",
  aside,
  children,
}: SectionProps) {
  return (
    <Card id={sectionElementId(id)} className="scroll-mt-24">
      <SectionHeader
        title={title}
        description={description}
        open={open}
        controls={panelElementId(id)}
        onToggle={onToggle}
        aside={aside}
      />
      <CardContent
        id={panelElementId(id)}
        hidden={!open}
        className="space-y-5"
      >
        {children}
        {specs === undefined ? null : specs.length === 0 ? (
          <p className="text-body-sm text-text-secondary">该分组暂无可用字段。</p>
        ) : (
          <div
            className={clsx(
              columns === "two"
                ? "grid gap-5 xl:grid-cols-2"
                : "space-y-5",
            )}
          >
            {specs.map((spec) => (
              <SettingField
                key={spec.key}
                spec={spec}
                value={values[spec.key]}
                diff={diffs[spec.key]}
                draft={drafts[spec.key]}
                error={errors[spec.key]}
                onChange={(raw) => onDraftChange(spec, raw)}
              />
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function InfoItem({
  label,
  children,
  mono = false,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
}) {
  return (
    <div className="min-w-0">
      <dt className="text-caption text-text-muted">{label}</dt>
      <dd
        className={clsx(
          "mt-0.5 text-body-sm break-all text-text",
          mono && "font-mono text-caption",
        )}
      >
        {children}
      </dd>
    </div>
  );
}

function ServiceDetails({ system }: { system: SystemResponse | undefined }) {
  if (!system) {
    return <p className="text-body-sm text-text-secondary">系统信息暂不可用。</p>;
  }
  return (
    <div className="space-y-3">
      {system.local_mode ? (
        <p
          role="status"
          className="rounded-card border border-warning/40 bg-warning/10 px-3 py-2 text-body-sm text-text"
        >
          本地模式 · API 无鉴权：任何能访问该端口的客户端都可修改设置。
        </p>
      ) : null}
      <dl className="grid gap-x-6 gap-y-3 sm:grid-cols-2 lg:grid-cols-3">
        <InfoItem label="版本">{system.version || "—"}</InfoItem>
        <InfoItem label="构建" mono>
          {system.build_commit || "dev"}
        </InfoItem>
        <InfoItem label="运行时长">{formatDuration(system.uptime_secs * 1_000)}</InfoItem>
        <InfoItem label="数据库路径" mono>
          {system.db_path || "—"}
        </InfoItem>
        <InfoItem label="数据库大小">{formatBytes(system.db_size_bytes)}</InfoItem>
        <InfoItem label="鉴权模式">{system.local_mode ? "本地模式" : "令牌 / 会话"}</InfoItem>
      </dl>
      <div>
        <p className="text-caption text-text-muted">服务单元</p>
        {system.services.length === 0 ? (
          <p className="mt-0.5 text-body-sm text-text-secondary">未配置 systemd unit。</p>
        ) : (
          <ul className="mt-1 divide-y divide-border rounded-card border border-border">
            {system.services.map((service) => (
              <li
                key={service.unit}
                className="flex min-w-0 items-center justify-between gap-3 px-3 py-2"
              >
                <code className="truncate font-mono text-caption text-text-secondary">
                  {service.unit}
                </code>
                <span
                  className={clsx(
                    "shrink-0 font-mono text-caption",
                    service.active === "n/a"
                      ? "text-text-muted"
                      : service.active === "active"
                        ? "text-success"
                        : "text-warning",
                  )}
                >
                  {service.active}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function AdvancedDetails({ data }: { data: SettingsResponse }) {
  return (
    <div className="space-y-4 border-t border-border pt-4">
      <dl className="grid gap-x-6 gap-y-3 sm:grid-cols-2">
        <InfoItem label="env 文件权限">
          {data.env_permissions ?? <span className="text-text-muted">n/a</span>}
        </InfoItem>
        <InfoItem label="env 文件状态">
          {data.env_exists ? "存在" : "尚不存在（首次保存时创建）"}
        </InfoItem>
      </dl>
      <div>
        <p className="text-caption text-text-muted">未识别的键（仅键名）</p>
        {data.unknown_keys.length === 0 ? (
          <p className="mt-0.5 text-body-sm text-text-secondary">无</p>
        ) : (
          <ul className="mt-1 flex flex-wrap gap-1.5">
            {data.unknown_keys.map((key) => (
              <li key={key}>
                <code className="rounded-sm border border-border bg-raised px-1.5 py-0.5 font-mono text-caption text-text-secondary">
                  {key}
                </code>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div>
        <p className="text-caption text-text-muted">最近变更</p>
        {data.last_changes.length === 0 ? (
          <p className="mt-0.5 text-body-sm text-text-secondary">暂无变更记录。</p>
        ) : (
          <ul className="mt-1 space-y-1">
            {data.last_changes.map((change) => (
              <li
                key={`${change.key}-${change.ts_ms}`}
                className="flex flex-wrap items-center gap-2 text-body-sm"
              >
                <code className="font-mono text-caption text-text">{change.key}</code>
                <time
                  dateTime={isoDateTime(change.ts_ms)}
                  className="font-mono text-caption text-text-secondary"
                >
                  {formatDateTime(change.ts_ms)}
                </time>
                <span className="text-caption text-text-muted">{change.actor ?? "系统"}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

interface RepoGroupCardProps {
  entry: RepoEntry;
  nowMs: number;
  onEdit: (repo: RepoDto) => void;
  onDelete: (repo: RepoDto) => void;
  onRegister: (prefill: RepoFormPrefill) => void;
}

function RepoGroupCard({ entry, nowMs, onEdit, onDelete, onRegister }: RepoGroupCardProps) {
  const { repo, registration, runners } = entry;
  const online = runners.filter((runner) => !runnerIsOffline(runner, nowMs)).length;
  const checkoutPaths = distinctStrings(runners.map((runner) => runner.repo_path));
  const failingChecks = distinctStrings(
    runners.map((runner) => runner.effective.failing_check),
  );
  const forbiddenCounts = distinctStrings(
    runners.map((runner) =>
      runner.effective.forbidden_paths_count === null
        ? null
        : String(runner.effective.forbidden_paths_count),
    ),
  );
  // `prs_today` is repo-scoped by the API (every runner of a repo reports the
  // same value), so sum limits but never sum today's count.
  const prsToday = Math.max(0, ...runners.map((runner) => runner.prs_today));
  const quotaLimits = distinctStrings(
    runners.map((runner) =>
      runner.daily_pr_limit !== null && runner.daily_pr_limit > 0
        ? String(runner.daily_pr_limit)
        : null,
    ),
  );
  const quota =
    quotaLimits.length > 0 ? `${prsToday}/${quotaLimits.join(" / ")}` : "未上报配额";
  const working = runners.filter((runner) => runner.current_fingerprint);
  const lastErrors = distinctStrings(runners.map((runner) => runner.last_error));
  const variant = online === runners.length ? "success" : online === 0 ? "danger" : "warning";
  const registerable = registration === null && repo !== UNREPORTED_REPO;

  return (
    <div
      data-testid={`repo-group-${repo}`}
      className="rounded-card border border-border"
    >
      <div className="flex flex-wrap items-start justify-between gap-2 border-b border-border px-3 py-2">
        <div className="min-w-0 flex-1 space-y-0.5">
          <div className="flex flex-wrap items-center gap-2">
            <p
              className={clsx(
                "min-w-0 text-body-sm font-semibold text-text",
                registration ? "break-words" : "break-all font-mono",
              )}
            >
              {registration ? registration.name : repo}
            </p>
            {registration ? null : <Badge variant="warning">未登记</Badge>}
            {runners.length > 0 ? (
              <Badge variant={variant}>
                {online}/{runners.length} 在线
              </Badge>
            ) : null}
          </div>
          {registration ? (
            <p className="break-all font-mono text-caption text-text-secondary">
              {repo}
            </p>
          ) : null}
          {registration ? (
            <>
              <p className="break-all text-caption text-text-muted">{registration.url}</p>
              {registration.description ? (
                <p className="break-words text-caption text-text-secondary">
                  {registration.description}
                </p>
              ) : null}
            </>
          ) : null}
        </div>
        <div className="flex flex-wrap items-center gap-1">
          {registration ? (
            <>
              <Button
                variant="secondary"
                size="sm"
                aria-label={`编辑 ${registration.name}`}
                onClick={() => onEdit(registration)}
              >
                <Pencil aria-hidden="true" className="size-3.5" />
                编辑
              </Button>
              <Button
                variant="ghost"
                size="sm"
                aria-label={`删除登记：${registration.name}`}
                onClick={() => onDelete(registration)}
              >
                <Trash2 aria-hidden="true" className="size-3.5" />
                删除
              </Button>
            </>
          ) : registerable ? (
            <Button
              variant="secondary"
              size="sm"
              onClick={() => onRegister({ repo, name: repo })}
            >
              登记
            </Button>
          ) : null}
        </div>
      </div>
      {runners.length === 0 ? (
        <div className="px-3 py-3">
          <p className="text-body-sm text-text-secondary">暂无 runner 上报</p>
          <p className="mt-0.5 text-caption text-text-muted">
            启动该仓库的 nuwa work 后，这里会显示检出路径、验证配置与配额。
          </p>
        </div>
      ) : (
        <>
          <dl className="grid gap-x-4 gap-y-3 px-3 py-3 sm:grid-cols-2 xl:grid-cols-4">
            <InfoItem label="检出路径" mono>
              {checkoutPaths.length > 0 ? checkoutPaths.join(" · ") : "—"}
            </InfoItem>
            <InfoItem label="Failing check" mono>
              {failingChecks.length > 0 ? failingChecks.join(" · ") : "—"}
            </InfoItem>
            <InfoItem label="禁改路径">
              {forbiddenCounts.length > 0 ? `${forbiddenCounts.join(" / ")} 个` : "—"}
            </InfoItem>
            <InfoItem label="今日 PR 配额" mono>
              {quota}
            </InfoItem>
            <InfoItem label="当前指纹">
              {working.length > 0 ? (
                <span className="flex flex-wrap gap-x-3 gap-y-1">
                  {working.map((runner) => (
                    <Link
                      key={runner.id}
                      to={issueDetailPath(runner.current_fingerprint ?? "")}
                      title={runner.current_fingerprint ?? undefined}
                      aria-label={`查看当前单：${runner.current_fingerprint}`}
                      className="inline-flex min-h-11 items-center rounded-sm font-mono text-text-secondary transition-colors hover:text-accent pointer-fine:min-h-9"
                    >
                      {shortFingerprint(runner.current_fingerprint ?? "")}
                    </Link>
                  ))}
                </span>
              ) : (
                "队列为空"
              )}
            </InfoItem>
          </dl>
          {lastErrors.length > 0 ? (
            <div className="mx-3 mb-3 rounded-card border border-danger/40 bg-danger/10 px-3 py-2">
              <p className="text-caption text-text-muted">最近错误</p>
              <ul className="mt-0.5 space-y-1">
                {lastErrors.map((error) => (
                  <li key={error} className="break-words font-mono text-caption text-text">
                    {error}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}

interface RepositoryBodyProps {
  reposQuery: ReturnType<typeof useReposQuery>;
  runnersQuery: ReturnType<typeof useRunnersQuery>;
  onEdit: (repo: RepoDto) => void;
  onDelete: (repo: RepoDto) => void;
  onRegister: (prefill: RepoFormPrefill) => void;
}

/**
 * Registry entries merged with the runner roster (docs/09 §8.7, §10.8). The
 * registry supplies the editable display identity; runners contribute the
 * read-only repository content. Runner-only identities stay visible with a
 * "未登记" badge and a prefilled registration shortcut.
 */
function RepositoryBody({
  reposQuery,
  runnersQuery,
  onEdit,
  onDelete,
  onRegister,
}: RepositoryBodyProps) {
  const registrations = reposQuery.data?.data ?? [];
  const runners = runnersQuery.data?.data ?? [];
  const entries = mergeRepoEntries(registrations, runners);
  const nowMs = Date.now();
  const loading =
    (reposQuery.isPending && !reposQuery.data) ||
    (runnersQuery.isPending && !runnersQuery.data);

  if (loading) {
    return (
      <div role="status" aria-busy="true" className="space-y-3">
        <span className="sr-only">正在加载仓库</span>
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-16 w-full" />
      </div>
    );
  }
  if (entries.length === 0 && runnersQuery.isError && !runnersQuery.data) {
    return (
      <PanelError
        title="无法加载仓库"
        error={runnersQuery.error}
        onRetry={() => void runnersQuery.refetch()}
      />
    );
  }
  return (
    <div className="space-y-3">
      {reposQuery.isError ? (
        <div className="flex flex-wrap items-start justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text">
          <p role="status" className="min-w-0 flex-1">
            仓库登记信息更新失败，登记状态可能不完整。
            <span className="ml-1 text-text-secondary">
              {describeError(reposQuery.error)}
            </span>
          </p>
          <Button variant="secondary" size="sm" onClick={() => void reposQuery.refetch()}>
            重试
          </Button>
        </div>
      ) : null}
      {runnersQuery.isError ? (
        <div className="flex flex-wrap items-start justify-between gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text">
          <p role="status" className="min-w-0 flex-1">
            运行器信息更新失败，仓库状态可能不完整。
            <span className="ml-1 text-text-secondary">
              {describeError(runnersQuery.error)}
            </span>
          </p>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void runnersQuery.refetch()}
          >
            重试
          </Button>
        </div>
      ) : null}
      {entries.length === 0 ? (
        <EmptyState
          icon={GitBranch}
          title="还没有仓库注册"
          description="仓库来自运行器心跳上报：在目标主机启动 nuwa work，首拍心跳后这里会按仓库汇总检出路径、验证配置与配额。也可以先手动登记仓库。"
        />
      ) : (
        entries.map((entry) => (
          <RepoGroupCard
            key={entry.repo}
            entry={entry}
            nowMs={nowMs}
            onEdit={onEdit}
            onDelete={onDelete}
            onRegister={onRegister}
          />
        ))
      )}
    </div>
  );
}

type RepoDialogState =
  | { mode: "create"; prefill?: RepoFormPrefill }
  | { mode: "edit"; target: RepoDto };

/**
 * Schema-driven settings editor (docs/09 §10.8, §13). Every group renders from
 * `schema`; the page only tracks drafts, so a refresh never discards an
 * editor's work, and the PUT carries only the keys they actually changed.
 */
export function SettingsPage() {
  const settings = useSettingsQuery();
  const system = useSystemQuery();
  const runners = useRunnersQuery();
  const repos = useReposQuery();
  const save = useSaveSettingsMutation();
  const testGithub = useTestSettingsMutation();
  const testLlm = useTestSettingsMutation();
  const removeRepo = useDeleteRepoMutation();
  const { toast } = useToast();
  const location = useLocation();
  const navigate = useNavigate();
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [pendingHref, setPendingHref] = useState<string | null>(null);
  const [restartOpen, setRestartOpen] = useState(false);
  const [repoDialog, setRepoDialog] = useState<RepoDialogState | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<RepoDto | null>(null);
  const [openSections, setOpenSections] = useState<Record<SectionId, boolean>>({
    ...INITIAL_OPEN,
  });
  const [lastSave, setLastSave] = useState<{
    changed: string[];
    restartRequired: boolean;
  } | null>(null);

  const data = settings.data;
  const dirtyCount = Object.keys(drafts).length;
  // Guards read dirty state through a ref, so listener setup never depends on
  // per-keystroke draft text.
  const dirtyRef = useRef(false);
  // Keys the last failed probe reported, so a later success can clear exactly
  // those inline errors without touching unrelated save validation.
  const probeErrorKeysRef = useRef<string[]>([]);

  useEffect(() => {
    dirtyRef.current = dirtyCount > 0;
  }, [dirtyCount]);

  // Native unload guard: the browser owns the confirmation copy.
  useEffect(() => {
    function handleBeforeUnload(event: BeforeUnloadEvent) {
      if (!dirtyRef.current) {
        return;
      }
      event.preventDefault();
      event.returnValue = "";
    }
    window.addEventListener("beforeunload", handleBeforeUnload);
    return () => window.removeEventListener("beforeunload", handleBeforeUnload);
  }, []);

  // In-app navigation guard: links are caught in the capture phase and
  // default-prevented, so the router's own click handler bails out (it checks
  // defaultPrevented) while other handlers, such as the drawer's close, still
  // run. Same-origin plain left clicks only; new tabs, downloads, external,
  // hash and same-location links pass.
  useEffect(() => {
    function handleLinkClick(event: MouseEvent) {
      if (!dirtyRef.current || event.defaultPrevented || event.button !== 0) {
        return;
      }
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
        return;
      }
      const target = event.target;
      if (!(target instanceof Element)) {
        return;
      }
      // Links inside a dialog this page owns (the restart dialog's active-run
      // fingerprints) navigate normally: the page unmounts and takes the
      // dialog with it. Guarding them would stack a second dialog on top of a
      // modal one. The shell's navigation drawer is also `role="dialog"` but
      // must still open the guard, so the exemption is explicit.
      if (target.closest('[role="dialog"][data-allow-navigation]')) {
        return;
      }
      const anchor = target.closest<HTMLAnchorElement>("a[href]");
      if (!anchor) {
        return;
      }
      if (anchor.target && anchor.target !== "_self") {
        return;
      }
      if (anchor.hasAttribute("download")) {
        return;
      }
      const href = anchor.getAttribute("href");
      if (!href || href.startsWith("#")) {
        return;
      }
      let url: URL;
      try {
        url = new URL(anchor.href, window.location.href);
      } catch {
        return;
      }
      if (url.origin !== window.location.origin) {
        return;
      }
      const samePathname =
        normalizePathname(url.pathname) === normalizePathname(location.pathname);
      const sameSearch = normalizeSearch(url.search) === normalizeSearch(location.search);
      const sameHash = url.hash === location.hash;
      if (samePathname && sameSearch && sameHash) {
        return;
      }
      // No stopPropagation: the drawer's NavLink onClick still runs and closes
      // the drawer while the guard dialog opens.
      event.preventDefault();
      setPendingHref(`${url.pathname}${url.search}${url.hash}`);
    }
    document.addEventListener("click", handleLinkClick, true);
    return () => document.removeEventListener("click", handleLinkClick, true);
  }, [location.pathname, location.search, location.hash]);

  function handleDraftChange(spec: SettingDto, raw: string) {
    setDrafts((current) => {
      const next = { ...current };
      if (draftIsUnchanged(spec, data?.values[spec.key], raw)) {
        delete next[spec.key];
      } else {
        next[spec.key] = raw;
      }
      return next;
    });
    setFieldErrors((current) => {
      if (!(spec.key in current)) {
        return current;
      }
      const next = { ...current };
      delete next[spec.key];
      return next;
    });
  }

  function handleDiscard() {
    setDrafts({});
    setFieldErrors({});
  }

  /** Unregistering only drops the console row; runners and the repo are untouched. */
  function handleRemoveRepo() {
    const target = deleteTarget;
    if (!target) {
      return;
    }
    removeRepo.mutate(target.repo, {
      onSuccess: () => {
        setDeleteTarget(null);
        toast({
          title: "已移除仓库登记",
          description: `${target.name}（${target.repo}）`,
          variant: "success",
        });
      },
      onError: (error) => {
        setDeleteTarget(null);
        toast({
          title: "移除失败",
          description: describeError(error),
          variant: "danger",
        });
      },
    });
  }

  function toggleSection(id: SectionId) {
    setOpenSections((current) => ({ ...current, [id]: !current[id] }));
  }

  function setAllSections(open: boolean) {
    setOpenSections(
      Object.fromEntries(NAV_ITEMS.map((item) => [item.id, open])) as Record<
        SectionId,
        boolean
      >,
    );
  }

  function handleSave() {
    if (!data) {
      return;
    }
    const specs = new Map(data.schema.map((spec) => [spec.key, spec]));
    const values: SettingsValues = {};
    const submittedDrafts: Record<string, string> = {};
    const errors: Record<string, string> = {};
    for (const [key, raw] of Object.entries(drafts)) {
      const spec = specs.get(key);
      if (!spec) {
        continue;
      }
      const message = validateSettingValue(spec, raw);
      if (message) {
        errors[key] = message;
        continue;
      }
      submittedDrafts[key] = raw;
      values[key] = spec.type === "int" && raw !== "" ? Number(raw) : raw;
    }
    if (Object.keys(errors).length > 0) {
      setFieldErrors(errors);
      toast({
        title: "无法保存",
        description: "请修正标记的字段后再保存。",
        variant: "danger",
      });
      return;
    }
    if (Object.keys(values).length === 0) {
      setDrafts({});
      return;
    }
    save.mutate(values, {
      onSuccess: (result) => {
        // Only drop the drafts that this PUT submitted and that still hold the
        // submitted text; an edit typed while the request was in flight stays.
        setDrafts((current) => {
          const next = { ...current };
          for (const [key, raw] of Object.entries(submittedDrafts)) {
            if (next[key] === raw) {
              delete next[key];
            }
          }
          return next;
        });
        setFieldErrors({});
        setLastSave({
          changed: result.changed,
          restartRequired: result.restart_required,
        });
        toast({
          title: "设置已保存",
          description:
            result.changed.length > 0
              ? `变更 ${result.changed.length} 项${result.restart_required ? "，重启后生效。" : "。"}`
              : "没有字段发生变化。",
          variant: "success",
        });
      },
      onError: (error) => {
        if (error instanceof ApiError && error.fields) {
          setFieldErrors(error.fields);
        }
        toast({
          title: "保存失败",
          description: describeError(error),
          variant: "danger",
        });
      },
    });
  }

  /** A probe that now succeeds retires the inline errors its last failure set. */
  function clearProbeErrors() {
    const keys = probeErrorKeysRef.current;
    if (keys.length === 0) {
      return;
    }
    probeErrorKeysRef.current = [];
    setFieldErrors((current) => {
      const next = { ...current };
      for (const key of keys) {
        delete next[key];
      }
      return next;
    });
  }

  function reportProbeError(error: unknown, title: string) {
    const fields = error instanceof ApiError ? error.fields : undefined;
    probeErrorKeysRef.current = fields ? Object.keys(fields) : [];
    if (fields) {
      // A 422 names the offending env keys: mirror them next to the fields and
      // keep the toast readable by joining every message.
      setFieldErrors((current) => ({ ...current, ...fields }));
    }
    const description = fields
      ? Object.entries(fields)
          .map(([key, message]) => `${key}：${message}`)
          .join("；")
      : describeError(error);
    toast({ title, description, variant: "danger" });
  }

  function handleTestGithub() {
    // A token typed into the reveal input is an unsaved candidate; the probe
    // must test it instead of the stored/environment value.
    const token = drafts.NUWA_GITHUB_TOKEN;
    testGithub.mutate(
      token
        ? { target: "github", values: { NUWA_GITHUB_TOKEN: token } }
        : { target: "github" },
      {
        onSuccess: (result) => {
          clearProbeErrors();
          toast({
            title: "GitHub token 可用",
            description: result.detail,
            variant: "success",
          });
        },
        onError: (error) => reportProbeError(error, "GitHub token 测试失败"),
      },
    );
  }

  function handleTestLlm() {
    const values: SettingsValues = {};
    for (const key of LLM_PROBE_KEYS) {
      const raw = drafts[key];
      if (raw) {
        values[key] = raw;
      }
    }
    testLlm.mutate(
      { target: "llm", values },
      {
        onSuccess: (result) => {
          clearProbeErrors();
          toast({
            title: "模型配置可用",
            description: result.detail,
            variant: "success",
          });
        },
        onError: (error) => reportProbeError(error, "模型配置测试失败"),
      },
    );
  }

  if (settings.isPending && !data) {
    return <SettingsSkeleton />;
  }
  if (!data) {
    return (
      <PanelError
        title="无法加载设置"
        error={settings.error}
        onRetry={() => void settings.refetch()}
      />
    );
  }

  const specsOf = (group: SettingGroup) => data.schema.filter((spec) => spec.group === group);
  const section = (group: SettingGroup) => SECTIONS.find((entry) => entry.group === group)!;
  const groupOfKey = new Map(data.schema.map((spec) => [spec.key, spec.group]));
  const dirtyGroups = new Set<SettingGroup>();
  for (const key of Object.keys(drafts)) {
    const group = groupOfKey.get(key);
    if (group) {
      dirtyGroups.add(group);
    }
  }
  // Only groups whose saved keys actually require a restart earn the nav badge.
  const restartGroups = new Set<SettingGroup>();
  if (lastSave?.restartRequired) {
    for (const key of lastSave.changed) {
      const group = groupOfKey.get(key);
      if (group) {
        restartGroups.add(group);
      }
    }
  }
  const sectionProps = (group: SettingGroup) => ({
    id: group,
    title: section(group).title,
    description: section(group).description,
    open: openSections[group],
    onToggle: () => toggleSection(group),
    specs: specsOf(group),
    values: data.values,
    diffs: data.effective_diff,
    drafts,
    errors: fieldErrors,
    onDraftChange: handleDraftChange,
  });
  const restartPending = data.restart_pending || lastSave?.restartRequired === true;

  const panelProps = {
    values: data.values,
    diffs: data.effective_diff,
    drafts,
    errors: fieldErrors,
    onDraftChange: handleDraftChange,
  };

  return (
    <div className="space-y-4">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-title font-semibold text-text">设置</h2>
          <p className="mt-1 text-body-sm text-text-secondary">
            按 schema 编辑 env 配置；保存只提交变更项，需重启的改动会单独提示。
          </p>
          <p className="mt-1 text-caption text-text-muted">
            密钥只显示末 4 位；留空保存表示保持当前值。
          </p>
        </div>
        {dirtyCount > 0 ? (
          <div
            data-testid="settings-header-actions"
            className="flex flex-wrap items-center gap-2"
          >
            <Badge variant="warning">{dirtyCount} 项未保存</Badge>
            <Button variant="ghost" size="sm" onClick={handleDiscard}>
              放弃更改
            </Button>
            <Button size="sm" pending={save.isPending} onClick={handleSave}>
              保存更改
            </Button>
          </div>
        ) : null}
      </header>

      <nav aria-label="设置分区（移动端）" className="lg:hidden">
        <div className="flex gap-2 overflow-x-auto pb-1">
          {NAV_ITEMS.map((item) => {
            const dirty = isSchemaGroup(item.id) && dirtyGroups.has(item.id);
            const restart = isSchemaGroup(item.id) && restartGroups.has(item.id);
            return (
              <a
                key={item.id}
                href={`#${sectionElementId(item.id)}`}
                className="inline-flex min-h-11 shrink-0 items-center gap-1.5 rounded-full border border-border bg-surface px-3 text-caption text-text-secondary transition-colors duration-150 ease-out hover:border-border-strong hover:text-text"
              >
                <span className="whitespace-nowrap">{item.label}</span>
                {dirty ? (
                  <span aria-hidden="true" className="size-2 shrink-0 rounded-full bg-warning" />
                ) : null}
                {dirty ? <span className="sr-only">有未保存更改</span> : null}
                {restart ? <Badge variant="warning">重启</Badge> : null}
              </a>
            );
          })}
        </div>
        <div className="mt-2 flex gap-2">
          <Button variant="ghost" size="sm" onClick={() => setAllSections(true)}>
            展开全部
          </Button>
          <Button variant="ghost" size="sm" onClick={() => setAllSections(false)}>
            收起全部
          </Button>
        </div>
      </nav>

      {settings.isError ? (
        <p
          role="status"
          className="rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text"
        >
          设置更新失败，正在显示上次数据。{describeError(settings.error)}
        </p>
      ) : null}

      {data.needs_migration ? (
        <p
          role="status"
          className="rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text"
        >
          检测到旧格式键名（小写），保存时会自动迁移为大写并保留备份。
        </p>
      ) : null}

      {restartPending ? (
        <div
          role="status"
          className="flex flex-wrap items-center gap-2 rounded-card border border-warning/40 bg-warning/10 px-4 py-2 text-body-sm text-text"
        >
          <Badge variant="warning">重启后生效</Badge>
          {lastSave && lastSave.changed.length > 0 ? (
            <>
              <span>已保存并等待重启：</span>
              {lastSave.changed.map((key) => (
                <code key={key} className="font-mono text-caption">
                  {key}
                </code>
              ))}
            </>
          ) : (
            <span>有配置更改尚未重启生效；重启 serve 与 runner units 后才会加载。</span>
          )}
          {lastSave?.changed.includes("NUWA_UI_TOKEN") ? (
            <span className="w-full text-warning">
              更改控制台令牌后，重启会使当前会话失效，需要重新登录。
            </span>
          ) : null}
        </div>
      ) : null}

      <div className="lg:grid lg:grid-cols-[220px_minmax(0,1fr)] lg:items-start lg:gap-6">
        <nav
          aria-label="设置分区"
          className="hidden lg:sticky lg:top-20 lg:block lg:self-start"
        >
          <div className="rounded-card border border-border bg-surface p-2">
            <ul className="space-y-0.5">
              {NAV_ITEMS.map((item) => {
                const dirty = isSchemaGroup(item.id) && dirtyGroups.has(item.id);
                const restart = isSchemaGroup(item.id) && restartGroups.has(item.id);
                return (
                  <li key={item.id}>
                    <a
                      href={`#${sectionElementId(item.id)}`}
                      className="flex min-h-11 items-center gap-2 rounded-card px-2.5 text-body-sm text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text"
                    >
                      <span className="min-w-0 flex-1 truncate">{item.label}</span>
                      {dirty ? (
                        <span
                          aria-hidden="true"
                          data-testid={`settings-nav-dirty-${item.id}`}
                          className="size-2 shrink-0 rounded-full bg-warning"
                        />
                      ) : null}
                      {dirty ? <span className="sr-only">有未保存更改</span> : null}
                      {restart ? (
                        <Badge
                          variant="warning"
                          data-testid={`settings-nav-restart-${item.id}`}
                        >
                          重启
                        </Badge>
                      ) : null}
                    </a>
                  </li>
                );
              })}
            </ul>
            <div className="mt-2 flex gap-2 border-t border-border pt-2">
              <Button
                variant="ghost"
                size="sm"
                className="flex-1"
                onClick={() => setAllSections(true)}
              >
                展开全部
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="flex-1"
                onClick={() => setAllSections(false)}
              >
                收起全部
              </Button>
            </div>
          </div>
        </nav>

        <div className="mt-4 space-y-4 lg:mt-0">
          <SettingsSection
            {...sectionProps("credentials")}
            aside={
              <Button
                variant="secondary"
                size="sm"
                pending={testGithub.isPending}
                onClick={handleTestGithub}
              >
                <ShieldCheck aria-hidden="true" className="size-3.5" />
                测试 GitHub token
              </Button>
            }
          />
          <SettingsSection
            {...sectionProps("model")}
            columns="two"
            aside={
              <Button
                variant="secondary"
                size="sm"
                pending={testLlm.isPending}
                onClick={handleTestLlm}
              >
                <FlaskConical aria-hidden="true" className="size-3.5" />
                测试模型配置
              </Button>
            }
          />
          <SettingsSection {...sectionProps("runner")} columns="two" />

          <SettingsSection
            id="repo"
            title="仓库"
            description="控制台登记的仓库与运行器心跳上报的仓库；登记信息可编辑，仓库内容只读。"
            open={openSections.repo}
            onToggle={() => toggleSection("repo")}
            {...panelProps}
            aside={
              <>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => setRepoDialog({ mode: "create" })}
                >
                  <Plus aria-hidden="true" className="size-3.5" />
                  添加仓库
                </Button>
                <Link
                  to="/runners"
                  className="inline-flex min-h-11 items-center gap-1.5 rounded-card border border-border-strong bg-raised px-3 text-caption font-medium text-text transition-colors duration-150 ease-out hover:border-accent hover:text-accent pointer-fine:min-h-9"
                >
                  查看运行器
                </Link>
              </>
            }
          >
            <RepositoryBody
              reposQuery={repos}
              runnersQuery={runners}
              onEdit={(target) => setRepoDialog({ mode: "edit", target })}
              onDelete={setDeleteTarget}
              onRegister={(prefill) => setRepoDialog({ mode: "create", prefill })}
            />
          </SettingsSection>

          <SettingsSection
            {...sectionProps("service")}
            aside={
              <>
                <SyncButton />
                <div className="flex flex-col items-end gap-1">
                  <Button
                    variant="danger"
                    size="sm"
                    // The post-recovery navigation would hit the beforeunload
                    // guard; force a save/discard decision before restarting.
                    disabled={dirtyCount > 0}
                    onClick={() => setRestartOpen(true)}
                  >
                    <Power aria-hidden="true" className="size-3.5" />
                    重启服务
                  </Button>
                  {dirtyCount > 0 ? (
                    <p className="text-caption text-warning">
                      请先保存或放弃未保存的更改
                    </p>
                  ) : null}
                </div>
              </>
            }
          >
            {system.isPending && !system.data ? (
              <Skeleton className="h-24 w-full" />
            ) : (
              <ServiceDetails system={system.data} />
            )}
          </SettingsSection>

          <SettingsSection {...sectionProps("security")} columns="two" />
          <SettingsSection {...sectionProps("ops")} columns="two" />
          <SettingsSection {...sectionProps("advanced")}>
            <AdvancedDetails data={data} />
          </SettingsSection>

          <SettingsSection
            id="about"
            title="关于"
            description="版本、许可证与源码位置。"
            open={openSections.about}
            onToggle={() => toggleSection("about")}
            {...panelProps}
          >
            <div className="space-y-2 text-body-sm text-text-secondary">
              <p>
                版本{" "}
                <code className="font-mono text-text">{system.data?.version ?? "—"}</code>
              </p>
              <p>
                许可证 <span className="text-text">MIT</span>
              </p>
              <p>
                源码：
                <a
                  href={REPO_URL}
                  target="_blank"
                  rel="noreferrer"
                  className="text-accent underline-offset-2 hover:underline"
                >
                  {REPO_URL}
                </a>
              </p>
            </div>
          </SettingsSection>

          {dirtyCount > 0 ? (
            <div
              data-testid="settings-save-bar"
              className="sticky bottom-14 z-20 -mx-4 border-t border-border bg-surface/95 px-4 py-3 backdrop-blur sm:-mx-6 lg:static lg:mx-0 lg:rounded-card lg:border"
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-body-sm text-text">{dirtyCount} 项未保存</p>
                <div className="flex gap-2">
                  <Button variant="ghost" size="sm" onClick={handleDiscard}>
                    放弃更改
                  </Button>
                  <Button size="sm" pending={save.isPending} onClick={handleSave}>
                    保存更改
                  </Button>
                </div>
              </div>
            </div>
          ) : null}
        </div>
      </div>

      <ConfirmDialog
        open={pendingHref !== null}
        title="放弃未保存的更改？"
        description={`离开后将丢失 ${dirtyCount} 项未保存的修改。`}
        confirmLabel="放弃并离开"
        cancelLabel="留在本页"
        danger
        onConfirm={() => {
          const href = pendingHref;
          setPendingHref(null);
          if (href) {
            navigate(href);
          }
        }}
        onCancel={() => setPendingHref(null)}
      />

      <RestartDialog
        open={restartOpen}
        onClose={() => setRestartOpen(false)}
        units={system.data?.services.map((service) => service.unit) ?? []}
      />

      <RepoFormDialog
        open={repoDialog !== null}
        mode={repoDialog?.mode ?? "create"}
        target={repoDialog?.mode === "edit" ? repoDialog.target : null}
        prefill={repoDialog?.mode === "create" ? (repoDialog.prefill ?? null) : null}
        onClose={() => setRepoDialog(null)}
      />

      <ConfirmDialog
        open={deleteTarget !== null}
        title="移除仓库登记？"
        description={
          deleteTarget
            ? `将从控制台移除 ${deleteTarget.name}（${deleteTarget.repo}）的登记。这不会停止 runner，也不会修改目标仓库的 nuwa.repo.json；仓库内容（failing_check、forbidden_paths、notes）保持不变。`
            : undefined
        }
        confirmLabel="移除登记"
        cancelLabel="取消"
        danger
        pending={removeRepo.isPending}
        cancelDisabled={removeRepo.isPending}
        onConfirm={handleRemoveRepo}
        onCancel={() => {
          if (!removeRepo.isPending) {
            setDeleteTarget(null);
          }
        }}
      />
    </div>
  );
}
