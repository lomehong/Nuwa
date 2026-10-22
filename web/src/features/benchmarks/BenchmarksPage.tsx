import { ArrowRight, FlaskConical, Gauge, RefreshCw, ShieldCheck } from "lucide-react";
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";

import { Sparkline, type SparklineDatum } from "../../components/charts/Sparkline";
import {
  Badge,
  Button,
  Card,
  CardContent,
  EmptyState,
  Select,
  Skeleton,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableRow,
} from "../../components/ui";
import { describeError } from "../../lib/errors";
import { CopyButton } from "../issue-detail/CopyButton";
import { formatDateTime, formatDuration, formatPercent, formatTokens } from "../overview/format";
import { useBenchmarksQuery } from "./queries";
import type { BenchmarkRunDto, BenchmarkStatus, ScenarioMeta } from "./types";

function statusVariant(status: BenchmarkStatus) {
  switch (status) {
    case "completed":
      return "success" as const;
    case "failed":
      return "danger" as const;
    default:
      return "info" as const;
  }
}

function statusLabel(status: BenchmarkStatus): string {
  switch (status) {
    case "completed":
      return "已完成";
    case "failed":
      return "失败";
    default:
      return "运行中";
  }
}

function passRate(run: BenchmarkRunDto): number | null {
  return run.scenarios > 0 ? run.repaired / run.scenarios : null;
}

function falsePositiveRate(run: BenchmarkRunDto): number | null {
  return run.claimed > 0 ? run.false_positive / run.claimed : null;
}

function deliveryRate(run: BenchmarkRunDto): number | null {
  return run.scenarios > 0 ? run.delivery_ok / run.scenarios : null;
}

function Metric({
  label,
  value,
  hint,
  tone,
}: {
  label: string;
  value: string;
  hint?: string;
  tone?: "danger" | "success";
}) {
  return (
    <div className="min-w-0 px-3 py-2">
      <p className="text-caption text-text-muted">{label}</p>
      <p
        className={
          tone === "danger"
            ? "mt-0.5 text-body font-semibold text-danger"
            : tone === "success"
              ? "mt-0.5 text-body font-semibold text-success"
              : "mt-0.5 text-body font-semibold text-text"
        }
      >
        {value}
      </p>
      {hint ? <p className="mt-0.5 text-caption text-text-secondary">{hint}</p> : null}
    </div>
  );
}

function ScenarioCard({ scenario }: { scenario: ScenarioMeta }) {
  return (
    <li>
      <Card>
        <CardContent className="space-y-2 p-4">
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge variant="info">{scenario.language}</Badge>
            <Badge variant={scenario.expect_fixed ? "success" : "warning"}>
              {scenario.expect_fixed ? "应修复" : "负对照"}
            </Badge>
          </div>
          <h3 className="font-mono text-body font-medium break-all text-text">{scenario.name}</h3>
          <p className="text-body-sm text-text-secondary">{scenario.title}</p>
          <p className="text-caption text-text-muted">{scenario.description}</p>
          <p className="font-mono text-caption break-all text-text-muted">
            检查：{scenario.failing_check}
          </p>
        </CardContent>
      </Card>
    </li>
  );
}

/**
 * 修复基准台账：最近运行的质量记分板 + 场景目录。
 *
 * 执行发生在 CLI/CI（`nuwa benchmark run`），控制台负责读取与对比——
 * 不在 serve 进程里跑模型循环，避免共享进程环境与密钥边界。
 */
export function BenchmarksPage() {
  const query = useBenchmarksQuery();
  const [mode, setMode] = useState<"all" | "faux" | "live">("all");
  const [status, setStatus] = useState<"all" | BenchmarkStatus>("all");

  const runs = query.data?.data ?? [];
  const scenarios = query.data?.scenarios ?? [];
  const latest = runs[0] ?? null;
  const previous = runs[1] ?? null;
  const filtered = useMemo(
    () =>
      runs.filter((run) => {
        if (mode !== "all" && run.mode !== mode) return false;
        if (status !== "all" && run.status !== status) return false;
        return true;
      }),
    [runs, mode, status],
  );

  const trend: SparklineDatum[] = useMemo(
    () =>
      [...runs]
        .reverse()
        .slice(-12)
        .map((run, index) => ({
          label: `第 ${index + 1} 次`,
          value: Math.round((passRate(run) ?? 0) * 100),
        })),
    [runs],
  );

  const latestDelta = latest && previous ? (passRate(latest) ?? 0) - (passRate(previous) ?? 0) : null;

  return (
    <section className="space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="flex items-center gap-2 text-title font-semibold tracking-tight text-text">
            <FlaskConical aria-hidden="true" className="size-5 text-accent" />
            修复基准
          </h2>
          <p className="mt-1 max-w-3xl text-body-sm text-text-secondary">
            固定种子场景 → 真实修复 → 验证门 → 交付门。通过率不是唯一指标：
            “声称修好但检查仍红”单列为误报，负对照场景专门验证门禁能拦住它。
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <code className="rounded-card border border-border bg-raised px-2 py-1 font-mono text-caption text-text-secondary">
            nuwa benchmark run --provider faux
          </code>
          <CopyButton
            value="nuwa benchmark run --provider faux"
            label="复制基准运行命令"
            toastTitle="已复制命令"
          />
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
          <span className="sr-only">正在加载基准运行</span>
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-40 w-full" />
        </div>
      ) : query.isError ? (
        <Card>
          <CardContent className="p-6">
            <p className="text-body-sm text-danger">基准列表加载失败：{describeError(query.error)}</p>
          </CardContent>
        </Card>
      ) : (
        <>
          <Card>
            <CardContent className="grid gap-2 p-2 sm:grid-cols-2 lg:grid-cols-5">
              <Metric
                label="最近通过率"
                value={formatPercent(latest ? passRate(latest) : null)}
                hint={
                  latestDelta === null
                    ? "还没有可对比的上一次"
                    : `较上次 ${latestDelta >= 0 ? "+" : ""}${(latestDelta * 100).toFixed(1)} 个百分点`
                }
                tone={latestDelta !== null && latestDelta < 0 ? "danger" : "success"}
              />
              <Metric
                label="误报率"
                value={formatPercent(latest ? falsePositiveRate(latest) : null)}
                hint={latest ? `${latest.false_positive} / ${latest.claimed} 次声称"修好"` : "—"}
                tone={latest && latest.false_positive > 0 ? "danger" : "success"}
              />
              <Metric
                label="交付门通过"
                value={formatPercent(latest ? deliveryRate(latest) : null)}
                hint={latest ? `${latest.delivery_ok}/${latest.scenarios} 个场景` : "—"}
              />
              <Metric
                label="成本"
                value={
                  latest
                    ? formatTokens(latest.tokens_uncached + latest.tokens_cached)
                    : "—"
                }
                hint={latest ? `${latest.turns} turns · ${formatDuration(latest.duration_ms)}` : "—"}
              />
              <Metric label="运行次数" value={String(runs.length)} hint="最多保留最近 50 次" />
            </CardContent>
          </Card>

          <div className="grid gap-4 lg:grid-cols-[1.2fr_1fr]">
            <Card>
              <CardContent className="p-4">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <h3 className="text-heading-sm font-semibold text-text">通过率趋势</h3>
                  <span className="text-caption text-text-muted">最近 {trend.length} 次运行</span>
                </div>
                <div className="mt-3">
                  <Sparkline
                    data={trend}
                    label="修复通过率"
                    unit="次"
                    valueFormatter={(value) => `${value}%`}
                    emptyLabel="还没有基准运行"
                  />
                </div>
              </CardContent>
            </Card>
            <Card>
              <CardContent className="p-4">
                <h3 className="text-heading-sm font-semibold text-text">怎么读这份记分板</h3>
                <ul className="mt-2 space-y-1.5 text-body-sm text-text-secondary">
                  <li>通过率 = 修复后检查变绿的场景 / 全部场景。</li>
                  <li>误报率 = 声称修好但检查仍红的次数 / 声称修好的次数。</li>
                  <li>负对照（应修复=false）通过意味着交付门正确拦截了误报。</li>
                  <li>真实模型（live）需要 API key，在 CLI/CI 中运行，控制台只读结果。</li>
                </ul>
              </CardContent>
            </Card>
          </div>

          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            <Select
              label="模式"
              value={mode}
              onChange={(event) => setMode(event.target.value as "all" | "faux" | "live")}
              options={[
                { value: "all", label: "全部模式" },
                { value: "faux", label: "faux（确定性）" },
                { value: "live", label: "live（真实模型）" },
              ]}
            />
            <Select
              label="状态"
              value={status}
              onChange={(event) => setStatus(event.target.value as "all" | BenchmarkStatus)}
              options={[
                { value: "all", label: "全部状态" },
                { value: "running", label: "运行中" },
                { value: "completed", label: "已完成" },
                { value: "failed", label: "失败" },
              ]}
            />
            <div className="flex items-end text-caption text-text-muted">
              显示 {filtered.length} / {runs.length} 次运行
            </div>
          </div>

          {runs.length === 0 ? (
            <Card>
              <CardContent className="p-6">
                <EmptyState
                  icon={Gauge}
                  title="还没有基准运行"
                  description="在仓库根运行 nuwa benchmark run --provider faux；结果会自动写入 shanhaijing.db，刷新此页即可看到。"
                />
              </CardContent>
            </Card>
          ) : filtered.length === 0 ? (
            <Card>
              <CardContent className="p-6">
                <EmptyState icon={Gauge} title="没有匹配的运行" description="调整模式或状态筛选。" />
              </CardContent>
            </Card>
          ) : (
            <Table
              caption="基准运行列表"
              label="基准运行列表"
              containerClassName="border-border"
            >
              <TableHead>
                <TableRow>
                  <TableHeaderCell>开始</TableHeaderCell>
                  <TableHeaderCell>状态</TableHeaderCell>
                  <TableHeaderCell>通过</TableHeaderCell>
                  <TableHeaderCell>误报</TableHeaderCell>
                  <TableHeaderCell>交付</TableHeaderCell>
                  <TableHeaderCell>成本</TableHeaderCell>
                  <TableHeaderCell>模式</TableHeaderCell>
                  <TableHeaderCell>详情</TableHeaderCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {filtered.map((run) => (
                  <TableRow key={run.id}>
                    <TableCell className="whitespace-nowrap">
                      <time
                        dateTime={new Date(run.started_at_ms).toISOString()}
                        title={formatDateTime(run.started_at_ms)}
                      >
                        {formatDateTime(run.started_at_ms)}
                      </time>
                    </TableCell>
                    <TableCell>
                      <Badge variant={statusVariant(run.status)}>{statusLabel(run.status)}</Badge>
                    </TableCell>
                    <TableCell>
                      <span className="font-mono tabular-nums">
                        {run.repaired}/{run.scenarios}
                      </span>
                      <span className="ml-1 text-caption text-text-muted">
                        {formatPercent(passRate(run))}
                      </span>
                    </TableCell>
                    <TableCell>
                      <span
                        className={
                          run.false_positive > 0
                            ? "font-mono tabular-nums text-danger"
                            : "font-mono tabular-nums"
                        }
                      >
                        {run.false_positive}
                      </span>
                    </TableCell>
                    <TableCell className="font-mono tabular-nums">
                      {run.delivery_ok}/{run.scenarios}
                    </TableCell>
                    <TableCell className="whitespace-nowrap font-mono tabular-nums">
                      {formatTokens(run.tokens_uncached + run.tokens_cached)}
                      <span className="ml-1 text-caption text-text-muted">
                        {formatDuration(run.duration_ms)}
                      </span>
                    </TableCell>
                    <TableCell>
                      <Badge variant={run.mode === "faux" ? "neutral" : "accent"}>
                        {run.mode}
                      </Badge>
                      <span className="ml-1 text-caption text-text-muted">
                        {run.provider}
                        {run.model ? `/${run.model}` : ""}
                      </span>
                    </TableCell>
                    <TableCell>
                      <Link
                        to={`/benchmarks/${encodeURIComponent(run.id)}`}
                        className="inline-flex items-center gap-1 text-body-sm text-accent transition-colors hover:underline"
                      >
                        查看
                        <ArrowRight aria-hidden="true" className="size-3.5" />
                      </Link>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}

          <div className="space-y-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h3 className="flex items-center gap-2 text-heading-sm font-semibold text-text">
                <ShieldCheck aria-hidden="true" className="size-4 text-accent" />
                场景目录
              </h3>
              <p className="text-caption text-text-muted">
                共 {scenarios.length} 个内置场景；负对照用于度量误报。
              </p>
            </div>
            {scenarios.length === 0 ? (
              <p className="text-body-sm text-text-muted">没有内置场景。</p>
            ) : (
              <ul className="grid gap-3 sm:grid-cols-2" aria-label="基准场景目录">
                {scenarios.map((scenario) => (
                  <ScenarioCard key={scenario.name} scenario={scenario} />
                ))}
              </ul>
            )}
          </div>
        </>
      )}
    </section>
  );
}
