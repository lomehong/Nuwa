import { clsx } from "clsx";
import { useState } from "react";

import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "../../components/ui";
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

/** Collapsed height before "展开全部"; roughly five events (docs/09 §10.5). */
const TIMELINE_MAX_H_CLASS = "max-h-96";

/**
 * Legacy databases logged one `reported` event per dedup hit, so a noisy
 * fingerprint could spend its whole 200-entry window on report spam. Fold
 * consecutive reported rows into one aggregated entry ("共 N 次", newest ts).
 */
function foldReported(items: TimelineItem[]): TimelineItem[] {
  const folded: TimelineItem[] = [];
  for (const item of items) {
    const last = folded[folded.length - 1];
    if (item.kind === "reported" && last?.kind === "reported") {
      last.count = (last.count ?? 1) + 1;
      continue;
    }
    folded.push({ ...item });
  }
  return folded;
}

interface IssueTimelineProps {
  items: TimelineItem[];
}

/**
 * Per-issue lifecycle timeline (docs/09 §10.5). The server returns newest
 * first; consecutive reported events fold into one row, and the list is
 * height-capped with an expand toggle so a long history cannot push the
 * evidence panel out of the audit viewport. System-owned fingerprints are
 * labelled as system events and never link to another issue.
 */
export function IssueTimeline({ items }: IssueTimelineProps) {
  const [expanded, setExpanded] = useState(false);
  const folded = foldReported(items);

  return (
    <IssueSection
      id="issue-timeline"
      title="时间线"
      description="状态迁移、PR、CI 与配置/重排事件，按时间倒序。"
      aside={
        <>
          {folded.length < items.length ? (
            <Badge>上报已聚合</Badge>
          ) : null}
          <Badge>最近 {items.length} 条</Badge>
        </>
      }
    >
      <Card>
        <CardHeader className="py-3">
          <CardTitle as="h4">生命周期事件</CardTitle>
          <span className="text-caption text-text-muted">相对时间悬停可看绝对时间</span>
        </CardHeader>
        <CardContent className="p-4">
          {folded.length === 0 ? (
            <p className="text-body-sm text-text-secondary">暂无时间线事件。</p>
          ) : (
            <>
              <div
                className={
                  expanded ? undefined : clsx(TIMELINE_MAX_H_CLASS, "overflow-y-auto pr-1")
                }
              >
                <ol className="relative space-y-4 border-l border-border pl-5">
                  {folded.map((item, index) => {
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
              </div>
              {items.length > 5 ? (
                <div className="mt-3 border-t border-border pt-2">
                  <Button variant="ghost" size="sm" onClick={() => setExpanded((v) => !v)}>
                    {expanded ? "收起时间线" : `展开全部 ${folded.length} 条`}
                  </Button>
                </div>
              ) : null}
            </>
          )}
        </CardContent>
      </Card>
    </IssueSection>
  );
}
