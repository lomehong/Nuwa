import { FlaskConical, Loader2, ShieldCheck } from "lucide-react";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  Navigate,
  useBlocker,
  useParams,
  type BlockerFunction,
} from "react-router-dom";

import { ConfirmDialog, Skeleton, useToast } from "../../components/ui";
import { ApiError } from "../../lib/api";
import { useAuth } from "../../lib/auth";
import { describeError } from "../../lib/errors";
import { PanelError } from "../overview/PanelChrome";
import {
  areaDirtySpecs,
  areaSpecByKey,
  areaSpecs,
  buildAreaSections,
  DEFAULT_SETTINGS_AREA,
  isSettingsAreaId,
  latestAreaChange,
  settingsAreaById,
  type SettingsAreaDef,
} from "./areas";
import { useSettingsDirtyReporter } from "./dirty";
import {
  useSaveSettingsMutation,
  useSettingsQuery,
  useTestSettingsMutation,
} from "./queries";
import { SettingsEffect } from "./SettingsEffect";
import { SettingsPanel } from "./SettingsPanel";
import {
  isPlainValue,
  type SettingDto,
  type SettingsValues,
  type SettingValueDto,
} from "./types";
import { validateSettingValue } from "./validation";

const LLM_PROBE_KEYS = [
  "NUWA_PROVIDER",
  "NUWA_MODEL",
  "NUWA_BASE_URL",
  "ZAI_API_KEY",
  "ANTHROPIC_API_KEY",
] as const;

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

function normalizePathname(pathname: string): string {
  const trimmed = pathname.replace(/\/+$/, "");
  return trimmed === "" ? "/" : trimmed;
}

function normalizeSearch(search: string): string {
  const params = new URLSearchParams(search);
  params.sort();
  return params.toString();
}

function SettingsSkeleton({ area }: { area: SettingsAreaDef }) {
  return (
    <div className="st-screen">
      <div className="st-content" role="status" aria-busy="true">
        <span className="sr-only">正在加载设置</span>
        <div className="st-head">
          <div>
            <h1>{area.label}</h1>
            <p className="st-desc">{area.description}</p>
          </div>
        </div>
        <div className="st-section">
          <div className="st-label">
            <Skeleton className="h-5 w-32" />
            <Skeleton className="mt-3 h-24 w-full" />
          </div>
          <Skeleton className="h-96 w-full rounded-modal" />
        </div>
      </div>
    </div>
  );
}

function SettingsAreaView({ area }: { area: SettingsAreaDef }) {
  const settings = useSettingsQuery();
  const save = useSaveSettingsMutation();
  const testGithub = useTestSettingsMutation();
  const testLlm = useTestSettingsMutation();
  const reportDirty = useSettingsDirtyReporter();
  const { localMode } = useAuth();
  const { toast } = useToast();
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [lastSave, setLastSave] = useState<{
    changed: string[];
  } | null>(null);
  const dirtyRef = useRef(false);
  const probeErrorKeysRef = useRef<string[]>([]);

  const data = settings.data;
  const specs = data ? areaSpecs(area, data.schema) : [];
  const specsByKey = data ? areaSpecByKey(area, data.schema) : new Map<string, SettingDto>();
  const dirtySpecs = data ? areaDirtySpecs(area, data.schema, drafts) : [];
  const dirtyCount = dirtySpecs.length;
  const restartDirty = dirtySpecs.filter((spec) => spec.requires_restart).length;
  const latestChange = data
    ? latestAreaChange(area, data.schema, data.last_changes)
    : null;

  // A route change to another area keeps this component mounted; drafts belong
  // to one view, so the new area always starts clean after the guard releases.
  useEffect(() => {
    setDrafts({});
    setFieldErrors({});
    setLastSave(null);
  }, [area.id]);

  useEffect(() => {
    dirtyRef.current = dirtyCount > 0;
  }, [dirtyCount]);

  useEffect(() => {
    reportDirty({ area: area.id, count: dirtyCount });
    return () => reportDirty({ area: null, count: 0 });
  }, [area.id, dirtyCount, reportDirty]);

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

  const shouldBlockNavigation = useCallback<BlockerFunction>(
    ({ currentLocation, nextLocation }) => {
      if (
        normalizePathname(currentLocation.pathname) ===
          normalizePathname(nextLocation.pathname) &&
        normalizeSearch(currentLocation.search) === normalizeSearch(nextLocation.search)
      ) {
        return false;
      }
      return dirtyRef.current;
    },
    [],
  );
  const blocker = useBlocker(shouldBlockNavigation);

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
    const values: SettingsValues = {};
    const submittedDrafts: Record<string, string> = {};
    const errors: Record<string, string> = {};
    for (const [key, raw] of Object.entries(drafts)) {
      const spec = specsByKey.get(key);
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
        });
        toast({
          title: "设置已保存",
          description:
            result.changed.length > 0
              ? `已写入 ${result.changed.length} 项；保存版本与进程状态以刷新后的服务端数据为准。`
              : result.restart_required
                ? "字段值未变，已重新登记保存版本；各进程加载状态以刷新后的数据为准。"
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
    const token = drafts.NUWA_GITHUB_TOKEN;
    testGithub.mutate(
      token
        ? { target: "github", candidate: "draft", values: { NUWA_GITHUB_TOKEN: token } }
        : { target: "github", candidate: "saved" },
      {
        onSuccess: (result) => {
          clearProbeErrors();
          toast({
            title: "GitHub 候选 token 可用",
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
      if (raw !== undefined) {
        values[key] = raw;
      }
    }
    testLlm.mutate(
      Object.keys(values).length > 0
        ? { target: "llm", candidate: "draft", values }
        : { target: "llm", candidate: "saved" },
      {
        onSuccess: (result) => {
          clearProbeErrors();
          toast({
            title: "模型候选配置可用",
            description: result.detail,
            variant: "success",
          });
        },
        onError: (error) => reportProbeError(error, "模型配置测试失败"),
      },
    );
  }

  if (settings.isPending && !data) {
    return <SettingsSkeleton area={area} />;
  }
  if (!data) {
    return (
      <div className="st-screen">
        <div className="st-content">
          <PanelError
            title="无法加载设置"
            error={settings.error}
            onRetry={() => void settings.refetch()}
          />
        </div>
      </div>
    );
  }

  const description = (
    <>
      {area.description} 配置写入 <code>{data.env_path}</code>。
    </>
  );
  const notices: Array<{ key: string; text: ReactNode }> = [];
  if (settings.isError) {
    notices.push({
      key: "stale",
      text: `设置更新失败，显示上次数据。${describeError(settings.error)}`,
    });
  }
  if (data.needs_migration) {
    notices.push({
      key: "migration",
      text: "检测到旧格式键名（小写），保存时会自动迁移为大写并保留备份。",
    });
  }
  if (localMode) {
    notices.push({
      key: "local",
      text: "本地模式 · API 无鉴权：任何能访问该端口的客户端都可修改设置。",
    });
  }
  if (lastSave?.changed.includes("NUWA_UI_TOKEN")) {
    notices.push({
      key: "ui-token",
      text: "更改控制台令牌后，重启会使当前会话失效，需要重新登录。",
    });
  }

  const runningLabel =
    area.id === "connection"
      ? "影响 work / serve"
      : area.id === "governance"
        ? "运行中 serve"
        : area.id === "environment"
          ? "本机环境"
          : "运行中 work";

  return (
    <div className="st-screen">
      <div className="st-content">
        <div className="st-head">
          <div>
            <h1>{area.label}</h1>
            <p className="st-desc">{description}</p>
            <p className="st-finding">
              {dirtyCount > 0 ? (
                <>
                  <span className="st-dot warn" aria-hidden="true" />
                  {dirtyCount} 项未保存
                  {restartDirty > 0 ? (
                    <>
                      {" · "}
                      <b>{restartDirty} 项需重启</b>
                    </>
                  ) : null}
                </>
              ) : (
                <>
                  <span className="st-dot ok" aria-hidden="true" />
                  没有未保存的更改
                </>
              )}
            </p>
          </div>
          <div className="st-head-right">
            <span className="st-stamp">{runningLabel}</span>
            {dirtyCount > 0 ? (
              <>
                <button
                  className="st-btn"
                  type="button"
                  onClick={handleDiscard}
                  disabled={save.isPending}
                >
                  放弃
                </button>
                <button
                  className="st-btn primary"
                  type="button"
                  onClick={handleSave}
                  disabled={save.isPending}
                  aria-busy={save.isPending || undefined}
                  data-testid="settings-save"
                >
                  {save.isPending ? (
                    <Loader2 aria-hidden="true" className="size-3.5 animate-spin" />
                  ) : null}
                  保存更改
                </button>
              </>
            ) : null}
          </div>
        </div>

        {notices.length > 0 ? (
          <div className="st-notices" role="status">
            {notices.map((notice) => (
              <p className="st-notice" key={notice.key}>
                <span className="st-dot warn" aria-hidden="true" />
                <span>{notice.text}</span>
              </p>
            ))}
          </div>
        ) : null}

        <div className="st-section">
          <div className="st-label">
            <h2>{area.sectionTitle}</h2>
            <p className="st-label-desc">{area.sectionDescription}</p>
            <SettingsEffect
              area={area}
              data={data}
              specs={specs}
              drafts={drafts}
              testActions={
                area.id === "connection" ? (
                  <>
                    <button
                      className="st-btn"
                      type="button"
                      aria-busy={testGithub.isPending || undefined}
                      disabled={testGithub.isPending || testLlm.isPending}
                      onClick={handleTestGithub}
                    >
                      {testGithub.isPending ? (
                        <Loader2 aria-hidden="true" className="size-3.5 animate-spin" />
                      ) : (
                        <ShieldCheck aria-hidden="true" className="size-3.5" />
                      )}
                      测试 GitHub token（{drafts.NUWA_GITHUB_TOKEN ? "草稿候选" : "已保存候选"}）
                    </button>
                    <button
                      className="st-btn"
                      type="button"
                      aria-busy={testLlm.isPending || undefined}
                      disabled={testGithub.isPending || testLlm.isPending}
                      onClick={handleTestLlm}
                    >
                      {testLlm.isPending ? (
                        <Loader2 aria-hidden="true" className="size-3.5 animate-spin" />
                      ) : (
                        <FlaskConical aria-hidden="true" className="size-3.5" />
                      )}
                      测试模型配置（{LLM_PROBE_KEYS.some((key) => drafts[key] !== undefined) ? "草稿候选" : "已保存候选"}）
                    </button>
                  </>
                ) : undefined
              }
            />
          </div>
          <SettingsPanel
            area={area}
            sections={buildAreaSections(area, data.schema)}
            data={data}
            drafts={drafts}
            errors={fieldErrors}
            latestChange={latestChange}
            onChange={handleDraftChange}
          />
        </div>
      </div>

      <ConfirmDialog
        open={blocker.state === "blocked"}
        title="放弃未保存的更改？"
        description={`离开后将丢失 ${dirtyCount} 项未保存的修改。`}
        confirmLabel="放弃并离开"
        cancelLabel="留在本页"
        danger
        onConfirm={() => {
          if (blocker.state === "blocked") {
            blocker.proceed();
          }
        }}
        onCancel={() => {
          if (blocker.state === "blocked") {
            blocker.reset();
          }
        }}
      />
    </div>
  );
}

export function SettingsPage() {
  const { area: areaParam } = useParams<{ area: string }>();
  const area = isSettingsAreaId(areaParam) ? settingsAreaById(areaParam) : null;
  if (!area) {
    return <Navigate to={`/settings/${DEFAULT_SETTINGS_AREA}`} replace />;
  }
  return <SettingsAreaView area={area} />;
}
