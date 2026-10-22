import { ArrowLeft, BookOpen, FolderGit2, RefreshCw } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";

import {
  Badge,
  Button,
  Card,
  CardContent,
  EmptyState,
  Input,
  Select,
  Skeleton,
} from "../../components/ui";
import { describeError } from "../../lib/errors";
import { useProjectSkillsQuery } from "./queries";
import type { SkillDto, SkillSource } from "./types";

const SOURCE_LABELS: Record<SkillSource, string> = {
  builtin: "内置",
  repo: "仓库",
};

function sourceVariant(source: SkillSource) {
  return source === "repo" ? "accent" : "neutral";
}

function SkillCard({ skill }: { skill: SkillDto }) {
  return (
    <li>
      <Card>
        <CardContent className="space-y-3 p-4">
          <div className="flex flex-wrap items-start justify-between gap-2">
            <div className="min-w-0">
              <h3 className="flex flex-wrap items-center gap-2 text-heading-sm font-semibold text-text">
                <span className="font-mono break-all">{skill.name}</span>
                <Badge variant="info">v{skill.version}</Badge>
                <Badge variant={sourceVariant(skill.source)}>{SOURCE_LABELS[skill.source]}</Badge>
                {skill.overrides_builtin ? <Badge variant="warning">覆盖内置</Badge> : null}
                {skill.allowlisted ? <Badge variant="accent">白名单</Badge> : null}
                {!skill.effective ? <Badge variant="neutral">已覆盖</Badge> : null}
              </h3>
              <p className="mt-1 text-body-sm text-text-secondary">{skill.description}</p>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-1.5 text-caption">
            {skill.languages.map((language) => (
              <Badge key={`lang-${language}`} variant="neutral">
                {language}
              </Badge>
            ))}
            {skill.matches.map((match) => (
              <Badge key={`match-${match}`} variant="info">
                命中 {match}
              </Badge>
            ))}
          </div>

          <p className="font-mono text-caption break-all text-text-muted" title={skill.origin}>
            来源：{skill.origin}
          </p>

          <details className="rounded-card border border-border bg-raised/40">
            <summary className="cursor-pointer px-3 py-2 text-body-sm text-text-secondary transition-colors hover:text-text">
              查看技能正文
            </summary>
            <pre className="max-h-96 overflow-auto border-t border-border px-3 py-2 font-mono text-caption whitespace-pre-wrap text-text">
              {skill.body}
            </pre>
            {skill.body_truncated ? (
              <p className="border-t border-border px-3 py-1.5 text-caption text-text-muted">
                正文过长，此处仅显示前 20,000 字符；运行器注入时另有更严格的长度上限。
              </p>
            ) : null}
          </details>
        </CardContent>
      </Card>
    </li>
  );
}

/**
 * 项目修复技能：内置技能 + 目标仓 `.nuwa/skills/*.md` 的只读目录。
 *
 * 仓库内容在控制台里保持只读（技能属于目标仓的代码资产）；这里展示
 * 运行器实际会看到的定义、来源、覆盖关系与解析告警。
 */
export function ProjectSkillsPage() {
  const repo = useParams<{ repo: string }>().repo ?? "";
  const query = useProjectSkillsQuery(repo);
  const [search, setSearch] = useState("");
  const [source, setSource] = useState<"all" | SkillSource>("all");
  const [language, setLanguage] = useState("all");
  const [effectiveOnly, setEffectiveOnly] = useState(true);

  const skills = query.data?.skills ?? [];
  const languages = useMemo(
    () => Array.from(new Set(skills.flatMap((skill) => skill.languages))).sort(),
    [skills],
  );
  const filtered = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return skills.filter((skill) => {
      if (effectiveOnly && !skill.effective) {
        return false;
      }
      if (source !== "all" && skill.source !== source) {
        return false;
      }
      if (language !== "all" && !skill.languages.includes(language)) {
        return false;
      }
      if (!needle) {
        return true;
      }
      return [skill.name, skill.description, skill.body, skill.origin]
        .join("\n")
        .toLowerCase()
        .includes(needle);
    });
  }, [skills, effectiveOnly, source, language, search]);

  const effectiveCount = skills.filter((skill) => skill.effective).length;
  const pinned = query.data?.selection_mode === "pinned";

  return (
    <section className="space-y-5">
      <header className="space-y-3">
        <Link
          to={`/projects/${encodeURIComponent(repo)}`}
          className="inline-flex min-h-11 items-center gap-1.5 text-body-sm text-text-secondary transition-colors hover:text-accent"
        >
          <ArrowLeft aria-hidden="true" className="size-4" />
          返回项目
        </Link>
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <h2 className="flex items-center gap-2 text-title font-semibold tracking-tight text-text">
              <BookOpen aria-hidden="true" className="size-5 text-accent" />
              修复技能
            </h2>
            <p className="mt-0.5 font-mono text-caption break-all text-text-muted">{repo}</p>
            <p className="mt-1 max-w-3xl text-body-sm text-text-secondary">
              技能是注入 runner 系统提示的排查/修复路径。选择是确定性的：仓库点名则按白名单，
              否则按问题语言与失败命令自动选择，最多 3 个。
            </p>
          </div>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void query.refetch()}
            disabled={query.isFetching}
          >
            <RefreshCw aria-hidden="true" className="size-3.5" />
            {query.isFetching ? "刷新中" : "刷新"}
          </Button>
        </div>
      </header>

      {query.isPending ? (
        <div className="space-y-3" role="status" aria-busy="true">
          <span className="sr-only">正在加载技能目录</span>
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-40 w-full" />
        </div>
      ) : query.isError ? (
        <Card>
          <CardContent className="p-6">
            <p className="text-body-sm text-danger">技能目录加载失败：{describeError(query.error)}</p>
          </CardContent>
        </Card>
      ) : (
        <>
          <Card>
            <CardContent className="grid gap-4 p-4 sm:grid-cols-3">
              <div>
                <p className="text-caption text-text-muted">选择模式</p>
                <p className="mt-1 text-body font-medium text-text">
                  {pinned ? "白名单钉住" : "自动选择"}
                </p>
                <p className="mt-1 text-caption text-text-secondary">
                  {pinned
                    ? "按 nuwa.repo.json 的 skills 顺序注入"
                    : "按语言与失败命令打分，最多 3 个"}
                </p>
              </div>
              <div>
                <p className="text-caption text-text-muted">生效技能</p>
                <p className="mt-1 text-body font-medium text-text">{effectiveCount} 个</p>
                <p className="mt-1 text-caption text-text-secondary">
                  共 {skills.length} 个定义（含被仓库覆盖的内置版本）
                </p>
              </div>
              <div className="min-w-0">
                <p className="text-caption text-text-muted">仓库路径</p>
                <p className="mt-1 truncate font-mono text-caption text-text-secondary" title={query.data?.repo_path ?? undefined}>
                  {query.data?.repo_path || "运行器未上报 repo_path"}
                </p>
                <p className="mt-1 flex items-center gap-1 text-caption text-text-muted">
                  <FolderGit2 aria-hidden="true" className="size-3" />
                  .nuwa/skills/*.md 由目标仓维护
                </p>
              </div>
              {query.data?.allowlist.length ? (
                <div className="sm:col-span-3">
                  <p className="text-caption text-text-muted">白名单</p>
                  <div className="mt-1 flex flex-wrap gap-1.5">
                    {query.data.allowlist.map((name) => (
                      <Badge key={name} variant="accent">
                        {name}
                      </Badge>
                    ))}
                  </div>
                </div>
              ) : null}
            </CardContent>
          </Card>

          {query.data?.warnings.length ? (
            <Card>
              <CardContent className="space-y-1 p-4">
                <h3 className="text-heading-sm font-semibold text-warning">解析告警</h3>
                {query.data.warnings.map((warning) => (
                  <p key={warning} className="text-body-sm break-words text-text-secondary">
                    · {warning}
                  </p>
                ))}
              </CardContent>
            </Card>
          ) : null}

          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <Input
              label="搜索技能"
              placeholder="名称、描述或正文"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            <Select
              label="来源"
              value={source}
              onChange={(event) => setSource(event.target.value as "all" | SkillSource)}
              options={[
                { value: "all", label: "全部来源" },
                { value: "builtin", label: "内置" },
                { value: "repo", label: "仓库" },
              ]}
            />
            <Select
              label="语言"
              value={language}
              onChange={(event) => setLanguage(event.target.value)}
              options={[
                { value: "all", label: "全部语言" },
                ...languages.map((entry) => ({ value: entry, label: entry })),
              ]}
            />
            <label className="flex min-h-11 items-center gap-2 self-end rounded-card border border-border bg-surface px-3 text-body-sm text-text-secondary">
              <input
                type="checkbox"
                checked={effectiveOnly}
                onChange={(event) => setEffectiveOnly(event.target.checked)}
                className="size-4 accent-accent"
              />
              只显示生效定义
            </label>
          </div>

          {filtered.length === 0 ? (
            <Card>
              <CardContent className="p-6">
                <EmptyState
                  icon={BookOpen}
                  title="没有匹配的技能"
                  description={
                    skills.length === 0
                      ? "内置技能不可用；请检查安装包是否完整。"
                      : "调整搜索、来源或语言筛选。"
                  }
                />
              </CardContent>
            </Card>
          ) : (
            <ul className="space-y-3" aria-label="技能目录">
              {filtered.map((skill) => (
                <SkillCard key={`${skill.source}:${skill.origin}`} skill={skill} />
              ))}
            </ul>
          )}

          <Card>
            <CardContent className="space-y-2 p-4 text-body-sm text-text-secondary">
              <h3 className="text-heading-sm font-semibold text-text">只读边界与校验</h3>
              <p>
                控制台不修改目标仓的技能文件：它们是仓库代码资产，应随 PR 评审。内置技能随二进制发布，
                仓库技能放在 <span className="font-mono">.nuwa/skills/*.md</span>，同名覆盖内置版本。
              </p>
              <p className="font-mono text-caption break-all text-text-muted">
                nuwa check --instructions &lt;repo&gt;/nuwa.repo.json
              </p>
            </CardContent>
          </Card>
        </>
      )}
    </section>
  );
}
