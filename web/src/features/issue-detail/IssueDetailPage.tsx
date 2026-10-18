import { useCallback } from "react";
import { useParams, useSearchParams } from "react-router-dom";

import { Badge } from "../../components/ui";
import { TranscriptPanel } from "../transcript/TranscriptPanel";
import { IssueDetailError, IssueDetailSkeleton, IssueDetailStaleNotice, IssueNotFound } from "./IssueDetailStates";
import { IssueEvidence } from "./IssueEvidence";
import { IssueHeader } from "./IssueHeader";
import { IssueRuns } from "./IssueRuns";
import { IssueSection } from "./IssueSection";
import { IssueSidebar } from "./IssueSidebar";
import { IssueTimeline } from "./IssueTimeline";
import { StackSample } from "./StackSample";
import { isNotFoundError, runOutcomeLabel, runPhaseLabel, shortRunId } from "./format";
import { useIssueDetailQuery } from "./queries";

/** Positive 1-based page; anything malformed falls back to 1. */
function parseTranscriptPage(raw: string | null): number {
  const value = Number.parseInt(raw ?? "1", 10);
  return Number.isFinite(value) && value > 0 ? value : 1;
}

/**
 * Issue detail + transcript (docs/09 §10.5).
 *
 * Layout: header, then a two-column main/sidebar grid that collapses to one
 * column below 1024px. The page polls the detail endpoint every 15s. The
 * transcript run selection and page live in the URL (`run_id`, `page`) so a
 * link restores exactly the audited conversation. M3 is read-only: no
 * reopen/requeue controls yet (M4).
 */
export function IssueDetailPage() {
  const { fp = "" } = useParams<{ fp: string }>();
  const [searchParams, setSearchParams] = useSearchParams();
  const query = useIssueDetailQuery(fp);
  const data = query.data;
  const page = parseTranscriptPage(searchParams.get("page"));
  const runId = searchParams.get("run_id");
  const initialLoading = query.isPending && !data;
  const notFound = isNotFoundError(query.error) && !data;

  const selectRun = useCallback(
    (nextRunId: string) => {
      setSearchParams((previous) => {
        const next = new URLSearchParams(previous);
        next.set("run_id", nextRunId);
        next.delete("page");
        return next;
      });
    },
    [setSearchParams],
  );

  const clearRun = useCallback(() => {
    setSearchParams((previous) => {
      const next = new URLSearchParams(previous);
      next.delete("run_id");
      next.delete("page");
      return next;
    });
  }, [setSearchParams]);

  const changeTranscriptPage = useCallback(
    (nextPage: number) => {
      setSearchParams((previous) => {
        const next = new URLSearchParams(previous);
        if (nextPage > 1) {
          next.set("page", String(nextPage));
        } else {
          next.delete("page");
        }
        return next;
      });
    },
    [setSearchParams],
  );

  if (initialLoading) {
    return <IssueDetailSkeleton />;
  }

  if (notFound) {
    return <IssueNotFound fingerprint={fp} />;
  }

  if (!data) {
    return (
      <IssueDetailError
        error={query.error}
        onRetry={() => void query.refetch()}
      />
    );
  }

  const latestRun = data.runs[0];
  const selectedRun = runId ? data.runs.find((run) => run.id === runId) : undefined;
  const transcriptLabel = runId
    ? selectedRun
      ? `当前会话：${shortRunId(runId)} · ${runPhaseLabel(selectedRun.phase)} · ${runOutcomeLabel(selectedRun.outcome)}`
      : `当前会话：${shortRunId(runId)}（不在最近 10 次运行中）`
    : "默认展示最近一次有会话的运行。";

  return (
    <article aria-labelledby="issue-title" className="space-y-6">
      {query.isRefetchError || (query.isError && data) ? (
        <IssueDetailStaleNotice
          error={query.error}
          onRetry={() => void query.refetch()}
        />
      ) : null}

      <IssueHeader issue={data.issue} />

      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_20rem] lg:items-start">
        <div className="min-w-0 space-y-6">
          <IssueTimeline items={data.timeline} />
          <IssueEvidence run={latestRun} />
          <StackSample text={data.stack_sample} />
          <IssueRuns
            runs={data.runs}
            selectedRunId={runId}
            onSelectRun={selectRun}
          />
          <IssueSection
            id="issue-transcript"
            title="会话转录"
            description="消息按 user / assistant / tool 分组；思考与工具结果默认折叠。"
            aside={<Badge>只读</Badge>}
          >
            <TranscriptPanel
              fingerprint={fp}
              runId={runId}
              page={page}
              runLabel={transcriptLabel}
              onPageChange={changeTranscriptPage}
              onClearRun={clearRun}
            />
          </IssueSection>
        </div>

        <aside className="min-w-0">
          <IssueSidebar
            issue={data.issue}
            latestRun={latestRun}
            similarCount={data.similar.count}
            similarItems={data.similar.items}
          />
        </aside>
      </div>
    </article>
  );
}
