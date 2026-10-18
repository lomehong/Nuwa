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
  onPageChange: (page: number) => void;
  onClearRun: () => void;
}

/**
 * Data container for the transcript viewer. It owns the query keyed by
 * fingerprint/run_id/page; the page owns the URL so a selected run and page
 * survive reloads and can be linked.
 */
export function TranscriptPanel({
  fingerprint,
  runId,
  page,
  runLabel,
  onPageChange,
  onClearRun,
}: TranscriptPanelProps) {
  const query = useTranscriptQuery(fingerprint, runId, page);

  return (
    <Card>
      <CardContent className="space-y-3 p-4">
        <p aria-live="polite" className="text-caption text-text-muted">
          {runLabel}
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
