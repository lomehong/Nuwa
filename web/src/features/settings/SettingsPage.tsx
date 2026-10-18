import { clsx } from "clsx";
import { FlaskConical, Power, ShieldCheck } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useLocation, useNavigate } from "react-router-dom";

import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  ConfirmDialog,
  Skeleton,
  useToast,
} from "../../components/ui";
import { ApiError } from "../../lib/api";
import { describeError } from "../../lib/errors";
import { SyncButton } from "../actions/SyncButton";
import { formatDateTime, formatDuration, isoDateTime } from "../overview/format";
import { PanelError } from "../overview/PanelChrome";
import { RestartDialog } from "./RestartDialog";
import { SettingField } from "./SettingField";
import {
  useSaveSettingsMutation,
  useSettingsQuery,
  useSystemQuery,
  useTestSettingsMutation,
} from "./queries";
import {
  isPlainValue,
  type DiffDto,
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
  title: string;
  description: string;
  specs: SettingDto[];
  values: Record<string, SettingValueDto>;
  diffs: Record<string, DiffDto>;
  drafts: Record<string, string>;
  errors: Record<string, string>;
  onDraftChange: (spec: SettingDto, raw: string) => void;
  aside?: ReactNode;
  children?: ReactNode;
}

function SettingsSection({
  title,
  description,
  specs,
  values,
  diffs,
  drafts,
  errors,
  onDraftChange,
  aside,
  children,
}: SectionProps) {
  return (
    <Card>
      <CardHeader>
        <div className="min-w-0">
          <CardTitle as="h2">{title}</CardTitle>
          <CardDescription>{description}</CardDescription>
        </div>
        {aside ? <div className="flex flex-wrap items-center gap-2">{aside}</div> : null}
      </CardHeader>
      <CardContent className="space-y-5">
        {children}
        {specs.length === 0 ? (
          <p className="text-body-sm text-text-secondary">该分组暂无可用字段。</p>
        ) : (
          <div className="space-y-5">
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

/**
 * Schema-driven settings editor (docs/09 §10.8, §13). Every group renders from
 * `schema`; the page only tracks drafts, so a refresh never discards an
 * editor's work, and the PUT carries only the keys they actually changed.
 */
export function SettingsPage() {
  const settings = useSettingsQuery();
  const system = useSystemQuery();
  const save = useSaveSettingsMutation();
  const testGithub = useTestSettingsMutation();
  const testLlm = useTestSettingsMutation();
  const { toast } = useToast();
  const location = useLocation();
  const navigate = useNavigate();
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [pendingHref, setPendingHref] = useState<string | null>(null);
  const [restartOpen, setRestartOpen] = useState(false);
  const [lastSave, setLastSave] = useState<{
    changed: string[];
    restartRequired: boolean;
  } | null>(null);

  const data = settings.data;
  const dirtyCount = Object.keys(drafts).length;
  // Guards read dirty state through a ref, so listener setup never depends on
  // per-keystroke draft text.
  const dirtyRef = useRef(false);

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
      const anchor =
        target instanceof Element ? target.closest<HTMLAnchorElement>("a[href]") : null;
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

  function reportProbeError(error: unknown, title: string) {
    const fields = error instanceof ApiError ? error.fields : undefined;
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
    testGithub.mutate(
      { target: "github" },
      {
        onSuccess: (result) => {
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
  const sectionProps = (group: SettingGroup) => ({
    title: section(group).title,
    description: section(group).description,
    specs: specsOf(group),
    values: data.values,
    diffs: data.effective_diff,
    drafts,
    errors: fieldErrors,
    onDraftChange: handleDraftChange,
  });
  const restartPending = data.restart_pending || lastSave?.restartRequired === true;

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
          <Badge variant="warning">未保存 {dirtyCount} 项</Badge>
        ) : null}
      </header>

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
      <SettingsSection {...sectionProps("runner")} />

      <SettingsSection
        {...sectionProps("service")}
        aside={
          <>
            <SyncButton />
            <Button
              variant="danger"
              size="sm"
              onClick={() => setRestartOpen(true)}
            >
              <Power aria-hidden="true" className="size-3.5" />
              重启服务
            </Button>
          </>
        }
      >
        {system.isPending && !system.data ? (
          <Skeleton className="h-24 w-full" />
        ) : (
          <ServiceDetails system={system.data} />
        )}
      </SettingsSection>

      <SettingsSection {...sectionProps("security")} />
      <SettingsSection {...sectionProps("ops")} />
      <SettingsSection {...sectionProps("advanced")}>
        <AdvancedDetails data={data} />
      </SettingsSection>

      <Card>
        <CardHeader>
          <div className="min-w-0">
            <CardTitle as="h2">关于</CardTitle>
            <CardDescription>版本、许可证与源码位置。</CardDescription>
          </div>
        </CardHeader>
        <CardContent className="space-y-2 text-body-sm text-text-secondary">
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
        </CardContent>
      </Card>

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
    </div>
  );
}
