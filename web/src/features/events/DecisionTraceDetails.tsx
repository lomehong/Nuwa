import { Copy } from "lucide-react";
import { useState } from "react";

import { Button } from "../../components/ui";
import { parseDecision } from "../../lib/eventKinds";

/**
 * Expandable raw trace for `decision` events. The one-line summary stays in
 * the row; this keeps the machine-readable `details` (skills, budgets,
 * commit/PR, errors) available without turning the feed into JSON soup.
 */
export function DecisionTraceDetails({ detail }: { detail: string }) {
  const [copied, setCopied] = useState(false);
  const parsed = parseDecision(detail);
  if (!parsed) {
    return null;
  }
  const pretty = JSON.stringify(parsed, null, 2);
  const detailEntries =
    parsed.details && typeof parsed.details === "object" && !Array.isArray(parsed.details)
      ? Object.entries(parsed.details as Record<string, unknown>)
      : [];
  const scalarDetails = detailEntries.filter(([, value]) =>
    ["string", "number", "boolean"].includes(typeof value),
  );

  async function copy() {
    if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
      return;
    }
    try {
      await navigator.clipboard.writeText(pretty);
      setCopied(true);
    } catch {
      // Clipboard denial is non-fatal; the text stays selectable in the <pre>.
    }
  }

  return (
    <details className="mt-1">
      <summary className="cursor-pointer text-caption text-text-secondary transition-colors hover:text-text">
        决策详情
      </summary>
      <div className="mt-1 space-y-2 rounded-card border border-border bg-raised/40 p-2">
        {scalarDetails.length ? (
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-caption">
            {scalarDetails.map(([key, value]) => (
              <div key={key} className="contents">
                <dt className="font-mono text-text-muted">{key}</dt>
                <dd className="font-mono break-all text-text-secondary">{String(value)}</dd>
              </div>
            ))}
          </dl>
        ) : null}
        <div className="relative">
          <pre className="max-h-64 overflow-auto font-mono text-caption whitespace-pre-wrap text-text-secondary">
            {pretty}
          </pre>
          <Button
            variant="ghost"
            size="sm"
            aria-label="复制决策 trace JSON"
            title="复制决策 trace JSON"
            className="absolute top-1 right-1"
            onClick={() => void copy()}
          >
            <Copy aria-hidden="true" className="size-3.5" />
            {copied ? "已复制" : null}
          </Button>
        </div>
      </div>
    </details>
  );
}
