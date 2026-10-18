import { Gem } from "lucide-react";
import { clsx } from "clsx";

export const SEVERITY_VALUES = ["critical", "high", "medium", "low", "unknown"] as const;

export type Severity = (typeof SEVERITY_VALUES)[number];

const SEVERITY_LABELS: Record<Severity, string> = {
  critical: "严重",
  high: "高",
  medium: "中",
  low: "低",
  unknown: "未分级",
};

export function severityLabel(severity: Severity): string {
  return SEVERITY_LABELS[severity];
}

export interface SeverityStoneProps {
  severity: Severity;
  /** Hide the text label only when the surrounding row already names it. */
  hideLabel?: boolean;
  className?: string;
}

/**
 * 五色石 severity marker. The colour is carried by the gem icon; the visible
 * label keeps the information available without colour vision and the
 * `sev-*` class is the only dynamic styling hook (strict CSP, no inline
 * styles).
 */
export function SeverityStone({ severity, hideLabel = false, className }: SeverityStoneProps) {
  return (
    <span
      className={clsx("severity-stone", `sev-${severity}`, className)}
      data-severity={severity}
    >
      <Gem aria-hidden="true" className="severity-stone__mark size-3.5" strokeWidth={2.25} />
      {hideLabel ? (
        <span className="sr-only">{severityLabel(severity)}</span>
      ) : (
        <span className="severity-stone__label">{severityLabel(severity)}</span>
      )}
    </span>
  );
}
