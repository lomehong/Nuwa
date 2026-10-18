import { Badge, Card, CardContent, CardHeader, CardTitle } from "../../components/ui";
import {
  actorLabel,
  eventKindLabel,
  eventKindVariant,
  formatAge,
  formatDateTime,
  isSystemFingerprint,
  isoDateTime,
} from "./format";
import { IssueSection } from "./IssueSection";
import type { TimelineItem } from "./types";

interface IssueTimelineProps {
  items: TimelineItem[];
}

/**
 * Per-issue lifecycle timeline (docs/09 §10.5). The server already returns
 * newest first; raw events carry no `count`, but an aggregated historical row
 * renders "共 N 次" instead of hiding the volume. System-owned fingerprints
 * are labelled as system events and never link to another issue.
 */
export function IssueTimeline({ items }: IssueTimelineProps) {
  return (
    <IssueSection
      id="issue-timeline"
      title="时间线"
      description="状态迁移、PR、CI 与配置/重排事件，按时间倒序。"
      aside={<Badge>最近 {items.length} 条</Badge>}
    >
      <Card>
        <CardHeader className="py-3">
          <CardTitle as="h4">生命周期事件</CardTitle>
          <span className="text-caption text-text-muted">相对时间悬停可看绝对时间</span>
        </CardHeader>
        <CardContent className="p-4">
          {items.length === 0 ? (
            <p className="text-body-sm text-text-secondary">暂无时间线事件。</p>
          ) : (
            <ol className="relative space-y-4 border-l border-border pl-5">
              {items.map((item, index) => {
                const actor = actorLabel(item.actor);
                const system = item.fingerprint
                  ? isSystemFingerprint(item.fingerprint)
                  : false;
                return (
                  <li
                    key={`${item.kind}-${item.ts_ms}-${index}`}
                    className="relative min-w-0"
                  >
                    <span
                      aria-hidden="true"
                      className="absolute top-1.5 -left-[1.45rem] size-2 rounded-full border border-surface bg-border-strong"
                    />
                    <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                      <Badge variant={eventKindVariant(item.kind)} title={item.kind}>
                        {eventKindLabel(item.kind)}
                      </Badge>
                      {item.count !== undefined && item.count > 1 ? (
                        <span className="text-caption font-medium whitespace-nowrap text-text-secondary">
                          共 {item.count} 次
                        </span>
                      ) : null}
                      {system ? (
                        <Badge variant="neutral" title={item.fingerprint}>
                          系统事件
                        </Badge>
                      ) : null}
                      <time
                        dateTime={isoDateTime(item.ts_ms)}
                        title={formatDateTime(item.ts_ms)}
                        className="text-caption whitespace-nowrap text-text-muted"
                      >
                        {formatAge(item.ts_ms)}
                      </time>
                    </div>
                    <p className="mt-1 text-body-sm break-words whitespace-pre-wrap text-text">
                      {item.detail || "—"}
                    </p>
                    <p className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-caption text-text-muted">
                      {actor ? <span>操作者：{actor}</span> : <span>操作者：未记录</span>}
                      <span aria-hidden="true">·</span>
                      <span className="font-mono" title={item.repo || undefined}>
                        {item.repo || "—"}
                      </span>
                    </p>
                  </li>
                );
              })}
            </ol>
          )}
        </CardContent>
      </Card>
    </IssueSection>
  );
}
