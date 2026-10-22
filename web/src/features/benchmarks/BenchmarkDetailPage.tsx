import { ArrowLeft, ArrowRight, CheckCircle2, CircleSlash, ShieldAlert } from "lucide-react";
import type { ReactNode } from "react";
import { Link, useParams } from "react-router-dom";

import {
  Badge,
  Card,
  CardContent,
  EmptyState,
  Skeleton,
} from "../../components/ui";
import { describeError } from "../../lib/errors";
import { formatDateTime, formatDuration, formatPercent, formatTokens } from "../overview/format";
import { useBenchmarkQuery, useBenchmarksQuery } from "./queries";
import type { BenchmarkResultDto, BenchmarkRunDto, BenchmarkStatus } from "./types";

function statusVariant(status: BenchmarkStatus) {
  return status === "completed" ? "success" : status === "failed" ? "danger" : "info";
}

function statusLabel(status: BenchmarkStatus): string {
  return status === "completed" ? "已完成" : status === "failed" ? "失败" : "运行中";
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

function Delta({ current, previous }: { current: number | null; previous: number | null }) {
  if (current === null || previous === null) {
    return <span className="text-caption text-text-muted">无可对比的上一次</span>;
  }
  const delta = (current - previous) * 100;
  return (
    <span
      className={
        delta < 0
          ? "text-caption text-danger"
          : delta > 0
            ? "text-caption text-success"
            : "text-caption text-text-muted"
      }
    >
      较上次 {delta >= 0 ? "+" : ""}
      {delta.toFixed(1)} 个百分点
    </span>
  );
}

function Metric({
  label,
  value,
  hint,
  tone,
}: {
  label: string;
  value: string;
  hint?: ReactNode;
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

function ResultBadge({
  ok,
  trueLabel,
  falseLabel,
}: {
  ok: boolean;
  trueLabel: string;
  falseLabel: string;
}) {
  return <Badge variant={ok ? "success" : "danger"}>{ok ? trueLabel : falseLabel}</Badge>;
}

function ResultCard({ result }: { result: BenchmarkResultDto }) {
  const falsePositive = result.claimed_fixed && !result.repaired;
  return (
    <li>
      <Card>
        <CardContent className="space-y-3 p-4">
          <div className="flex flex-wrap items-start justify-between gap-2">
            <div className="min-w-0">
              <h3 className="font-mono text-body font-medium break-all text-text">
                {result.scenario}
              </h3>
              <div className="mt-1 flex flex-wrap items-center gap-1.5">
                <Badge variant="info">{result.language}</Badge>
                <Badge variant={result.expected_fixed ? "success" : "warning"}>
                  预期 {result.expected_fixed ? "修复" : "不修复"}
                </Badge>
                <ResultBadge ok={result.repaired} trueLabel="已修复" falseLabel="未修复" />
                {result.claimed_fixed ? <Badge variant="accent">声称修好</Badge> : null}
                {falsePositive ? <Badge variant="danger">误报</Badge> : null}
                {!result.matches_expectation ? (
                  <Badge variant="warning">与预期不符</Badge>
                ) : null}
              </div>
            </div>
            <div className="text-right text-caption text-text-secondary">
              <p className="font-mono tabular-nums">
                {result.turns} turns · {formatTokens(result.tokens_uncached + result.tokens_cached)}
              </p>
              <p className="font-mono tabular-nums">{formatDuration(result.duration_ms)}</p>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2 text-caption">
            <span className="text-text-muted">交付门：</span>
            <ResultBadge ok={result.delivery_ok} trueLabel="通过" falseLabel="拦截/失败" />
            {falsePositive && !result.delivery_ok ? (
              <span className="inline-flex items-center gap-1 text-danger">
                <ShieldAlert aria-hidden="true" className="size-3.5" />
                门禁正确拦截了这次误报
              </span>
            ) : null}
          </div>

          {result.error ? (
            <p className="rounded-card border border-danger/40 bg-danger/10 px-2 py-1 text-caption break-words text-text">
              错误：{result.error}
            </p>
          ) : null}

          {result.root_cause ? (
            <details>
              <summary className="cursor-pointer text-caption text-text-secondary hover:text-text">
                根因与最终报告
              </summary>
              <pre className="mt-1 max-h-64 overflow-auto font-mono text-caption whitespace-pre-wrap text-text-secondary">
                {result.root_cause}
              </pre>
            </details>
          ) : null}

          {result.final_report && result.final_report !== result.root_cause ? (
            <details>
              <summary className="cursor-pointer text-caption text-text-secondary hover:text-text">
                原始最终消息
              </summary>
              <pre className="mt-1 max-h-64 overflow-auto font-mono text-caption whitespace-pre-wrap text-text-secondary">
                {result.final_report}
              </pre>
            </details>
          ) : null}
        </CardContent>
      </Card>
    </li>
  );
}

export function BenchmarkDetailPage() {
  const id = useParams<{ id: string }>().id ?? "";
  const query = useBenchmarkQuery(id);
  const list = useBenchmarksQuery();
  const run = query.data?.run;
  const results = query.data?.results ?? [];
  const previous =
    run && list.data
      ? (list.data.data.find((entry) => entry.started_at_ms < run.started_at_ms) ?? null)
      : null;
  const mismatches = results.filter((result) => !result.matches_expectation).length;

  return (
    <section className="space-y-5">
      <header className="space-y-3">
        <Link
          to="/benchmarks"
          className="inline-flex min-h-11 items-center gap-1.5 text-body-sm text-text-secondary transition-colors hover:text-accent"
        >
          <ArrowLeft aria-hidden="true" className="size-4" />
          修复基准
        </Link>
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <h2 className="text-title font-semibold tracking-tight text-text">基准运行</h2>
            <p className="mt-0.5 font-mono text-caption break-all text-text-muted">{id}</p>
            {run ? (
              <div className="mt-1 flex flex-wrap items-center gap-1.5">
                <Badge variant={statusVariant(run.status)}>{statusLabel(run.status)}</Badge>
                <Badge variant={run.mode === "faux" ? "neutral" : "accent"}>{run.mode}</Badge>
                <span className="text-caption text-text-secondary">
                  {run.provider}
                  {run.model ? `/${run.model}` : ""}
                </span>
                <span className="text-caption text-text-muted">
                  开始于 {formatDateTime(run.started_at_ms)}
                </span>
              </div>
            ) : null}
          </div>
        </div>
      </header>

      {query.isPending ? (
        <div className="space-y-3" role="status" aria-busy="true">
          <span className="sr-only">正在加载基准详情</span>
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-40 w-full" />
        </div>
      ) : query.isError ? (
        <Card>
          <CardContent className="p-6">
            <p className="text-body-sm text-danger">基准详情加载失败：{describeError(query.error)}</p>
          </CardContent>
        </Card>
      ) : !run ? (
        <Card>
          <CardContent className="p-6">
            <EmptyState icon={CircleSlash} title="找不到这次运行" description="它可能已被清理。" />
          </CardContent>
        </Card>
      ) : (
        <>
          {run.status === "running" ? (
            <Card>
              <CardContent className="p-4 text-body-sm text-text-secondary">
                这次运行仍在进行；页面每 5 秒刷新一次。已完成的场景会先出现在下方。
              </CardContent>
            </Card>
          ) : null}

          {run.error ? (
            <Card>
              <CardContent className="p-4">
                <p className="text-body-sm break-words text-danger">运行错误：{run.error}</p>
              </CardContent>
            </Card>
          ) : null}

          <Card>
            <CardContent className="grid gap-2 p-2 sm:grid-cols-2 lg:grid-cols-5">
              <Metric
                label="通过率"
                value={formatPercent(passRate(run))}
                hint={
                  <Delta current={passRate(run)} previous={previous ? passRate(previous) : null} />
                }
                tone={run.repaired < run.scenarios ? "danger" : "success"}
              />
              <Metric
                label="误报"
                value={`${run.false_positive} 次`}
                hint={`${run.claimed} 次声称修好 · ${formatPercent(falsePositiveRate(run))}`}
                tone={run.false_positive > 0 ? "danger" : "success"}
              />
              <Metric
                label="交付门"
                value={`${run.delivery_ok}/${run.scenarios}`}
                hint={formatPercent(deliveryRate(run))}
              />
              <Metric
                label="预期偏差"
                value={`${mismatches} 个场景`}
                hint="负对照应保持未修复；正场景应修复"
                tone={mismatches > 0 ? "danger" : "success"}
              />
              <Metric
                label="成本"
                value={formatTokens(run.tokens_uncached + run.tokens_cached)}
                hint={`${run.turns} turns · ${formatDuration(run.duration_ms)}`}
              />
            </CardContent>
          </Card>

          <div className="flex flex-wrap items-center justify-between gap-2">
            <h3 className="text-heading-sm font-semibold text-text">
              场景结果（{results.length}/{run.scenarios}）
            </h3>
            {previous ? (
              <Link
                to={`/benchmarks/${encodeURIComponent(previous.id)}`}
                className="inline-flex items-center gap-1 text-caption text-text-secondary transition-colors hover:text-accent"
              >
                对比上一次
                <ArrowRight aria-hidden="true" className="size-3.5" />
              </Link>
            ) : null}
          </div>

          {results.length === 0 ? (
            <Card>
              <CardContent className="p-6">
                <EmptyState
                  icon={CheckCircle2}
                  title={run.status === "running" ? "场景尚未完成" : "没有场景结果"}
                  description="运行完成后每个场景会写入一条结果。"
                />
              </CardContent>
            </Card>
          ) : (
            <ul className="space-y-3" aria-label="基准场景结果">
              {results.map((result) => (
                <ResultCard key={result.scenario} result={result} />
              ))}
            </ul>
          )}
        </>
      )}
    </section>
  );
}
