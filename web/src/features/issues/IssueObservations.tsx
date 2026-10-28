import { Badge, Card, CardContent } from "../../components/ui";
import type { IssueDetailResponse } from "../../lib/api-types/IssueDetailResponse";
import type { SignalObservationDto } from "../../lib/api-types/SignalObservationDto";
import { formatDateTime, isoDateTime } from "../overview/format";
import { IssueSection } from "../issue-detail/IssueSection";

const OBSERVATIONS_LIMIT = 100;

const TIME_STATUS_LABELS: Record<string, string> = {
  missing: "未提供",
  invalid: "无效",
  future: "晚于接收时间",
  untrusted: "未授信",
  trusted: "已授信",
};

const DISPOSITION_LABELS: Record<string, string> = {
  new: "首次接收",
  deduped: "重复上报（聚合）",
  cooldown: "冷却期上报",
  regression: "回归上报",
  delayed: "延迟上报",
  duplicate: "重复事件",
  muted: "已静默",
};

type ObservationDetail = Partial<Pick<
  IssueDetailResponse,
  "observations" | "deployment_status" | "recovery_status"
>>;

function evidenceStatus(status: string | undefined): string {
  return !status || status === "unknown" ? "未知" : `未知（未支持的状态：${status}）`;
}

function ReceiptTime({ value }: { value: number | null | undefined }) {
  if (value == null) return <>未提供</>;
  const label = formatDateTime(value);
  return value <= 0 || label === "—" ? (
    <>无效时间（{value} ms）</>
  ) : (
    <time dateTime={isoDateTime(value)}>{label}</time>
  );
}

function Observation({ item }: { item: SignalObservationDto }) {
  const metadata: Array<[string, string | null | undefined]> = [
    ["环境", item.environment],
    ["发布版本 SHA", item.release_sha],
    ["来源事件 ID", item.source_event_id],
    ["授信来源", item.trusted_source],
  ];

  return (
    <li className="min-w-0 space-y-2 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-caption font-medium text-text-secondary">回执 #{item.id}</span>
        <Badge title={item.disposition}>
          {DISPOSITION_LABELS[item.disposition] ?? (item.disposition || "处置未记录")}
        </Badge>
        <Badge variant={item.trusted_source ? "info" : "neutral"}>
          {item.trusted_source ? "来源已授信" : "来源未授信"}
        </Badge>
      </div>
      <dl className="grid gap-x-4 gap-y-1 text-body-sm sm:grid-cols-[auto_minmax(0,1fr)]">
        <dt className="text-text-muted">接收时间</dt>
        <dd className="text-text"><ReceiptTime value={item.received_at_ms} /></dd>
        <dt className="text-text-muted">采集时间</dt>
        <dd className="text-text"><ReceiptTime value={item.observed_at_ms} /></dd>
        <dt className="text-text-muted">采集时间状态</dt>
        <dd className="break-all text-text" title={item.time_status}>
          {TIME_STATUS_LABELS[item.time_status] ?? (item.time_status || "未记录")}
        </dd>
        {metadata.map(([label, value]) => (
          <Metadata key={label} label={label} value={value} />
        ))}
        {item.duplicate_of != null ? (
          <Metadata label="重复自回执" value={`#${item.duplicate_of}`} />
        ) : null}
      </dl>
    </li>
  );
}

function Metadata({ label, value }: { label: string; value: string | null | undefined }) {
  return (
    <>
      <dt className="text-text-muted">{label}</dt>
      <dd className="font-mono break-all whitespace-pre-wrap text-text">{value || "未提供"}</dd>
    </>
  );
}

/** Receipts are not lifecycle events or proof that a merged fix is live. */
export function IssueObservations({ detail }: { detail: ObservationDetail }) {
  const items = (detail.observations ?? []).slice(0, OBSERVATIONS_LIMIT);

  return (
    <IssueSection
      id="issue-observations"
      title="信号回执"
      description="按接收时间倒序，最多显示最近 100 条；接收时间由服务端记录，采集时间与环境/版本来自上报。"
      aside={<Badge>最近 {items.length} 条</Badge>}
    >
      <Card>
        <CardContent className="p-4">
          <div className="mb-3 space-y-1 border-b border-border pb-3 text-body-sm text-text-secondary">
            <p>部署状态：{evidenceStatus(detail.deployment_status)}</p>
            <p>恢复状态：{evidenceStatus(detail.recovery_status)}</p>
            <p>PR 已合并不等于已部署，也不等于运行时已恢复；尚未接入部署或健康证据。</p>
            <p>来源授信与采集时间状态分别记录；上报的环境/版本不构成部署或恢复证明。</p>
          </div>
          {items.length === 0 ? (
            <p className="text-body-sm text-text-secondary">暂无信号回执；旧问题单或已过保留期的记录可能没有回执历史。</p>
          ) : (
            <div role="region" aria-label="回执记录" tabIndex={0} className="max-h-96 overflow-y-auto pr-1">
              <ol className="divide-y divide-border">
                {items.map((item) => <Observation key={item.id} item={item} />)}
              </ol>
            </div>
          )}
        </CardContent>
      </Card>
    </IssueSection>
  );
}