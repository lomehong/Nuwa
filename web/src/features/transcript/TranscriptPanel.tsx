import { Card, CardContent } from "../../components/ui";
import { useTranscriptQuery } from "./queries";
import { TranscriptViewer } from "./TranscriptViewer";

export interface TranscriptPanelProps {
  fingerprint: string;
  /** `run_id` from the URL; `null` means the newest run with a session. */
  runId: string | null;
  page: number;
  /** Human label for the selected (or default) run. */
  runLabel: string;
  /** Poll cadence while the viewed run is still in flight; `null` = static. */
  pollMs: number | null;
  onPageChange: (page: number) => void;
  onClearRun: () => void;
}

/**
 * Data container for the transcript viewer. It owns the query keyed by
 * fingerprint/run_id/page; the page owns the URL so a selected run and page
 * survive reloads and can be linked. While the viewed run is unfinished the
 * query polls, so the "陪跑" view grows new messages without a manual reload.
 */
export function TranscriptPanel({
  fingerprint,
  runId,
  page,
  runLabel,
  pollMs,
  onPageChange,
  onClearRun,
}: TranscriptPanelProps) {
  const query = useTranscriptQuery(fingerprint, runId, page, {
    refetchIntervalMs: pollMs ?? undefined,
  });

  return (
    <Card>
      <CardContent className="space-y-3 p-4">
        <p aria-live="polite" className="text-caption text-text-muted">
          {runLabel}
          {pollMs ? " · 运行进行中，自动刷新" : ""}
        </p>
        <TranscriptViewer
          response={query.data}
          isPending={query.isPending && !query.data}
          isFetching={query.isFetching}
          error={query.error}
          runId={runId}
          page={page}
          onPageChange={onPageChange}
          onRetry={() => void query.refetch()}
          onClearRun={onClearRun}
        />
      </CardContent>
    </Card>
  );
}
