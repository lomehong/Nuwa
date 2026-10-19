import { ExternalLink } from "lucide-react";

import { Badge, type BadgeVariant } from "../../components/ui";

const CI_VERDICTS: Record<string, { label: string; variant: BadgeVariant }> = {
  success: { label: "CI 通过", variant: "success" },
  failure: { label: "CI 失败", variant: "danger" },
  timeout: { label: "CI 超时", variant: "warning" },
  unavailable: { label: "CI 不可读", variant: "warning" },
  pending: { label: "CI 待定", variant: "info" },
};

/** Server-side stuck flag; text + icon-tinted badge, never colour alone.
 * "疑似" is the operative word — the flag is a heuristic (docs/09 §8.4). */
export function StuckBadge() {
  return <Badge variant="warning">疑似卡单</Badge>;
}

interface PrLinkProps {
  url: string;
  title: string;
}

export function PrLink({ url, title }: PrLinkProps) {
  return (
    <a
      href={url}
      target="_blank"
      rel="noreferrer"
      aria-label={`打开 PR：${title}`}
      className="inline-flex min-h-6 items-center gap-1 rounded-sm text-caption font-medium text-info transition-colors hover:text-accent"
    >
      PR
      <ExternalLink aria-hidden="true" className="size-3" />
    </a>
  );
}

interface CiVerdictBadgeProps {
  verdict: string | null;
}

/** Newest-run CI verdict; unknown server values degrade to a neutral label. */
export function CiVerdictBadge({ verdict }: CiVerdictBadgeProps) {
  if (!verdict) {
    return null;
  }
  const known = CI_VERDICTS[verdict];
  return <Badge variant={known?.variant ?? "neutral"}>{known?.label ?? verdict}</Badge>;
}
