import { useMutation, useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";

import { Button, useToast } from "../../components/ui";
import { ApiError, apiFetch } from "../../lib/api";
import { describeError } from "../../lib/errors";
import { invalidateIssueViews, invalidateSettingsViews } from "../../lib/queryInvalidation";
import { syncErrorText, type SyncResponse } from "./types";

const SYNC_CONFLICT = "已有对账任务在运行，请稍后重试。";
/** Per-issue failures are detail, not headline: auto-dismiss after 6s. */
const ERROR_TOAST_MS = 6000;
const MAX_ERROR_TOASTS = 3;

interface SyncButtonProps {
  className?: string;
}

/**
 * Owner-triggered GitHub reconcile (docs/09 §9.2, §10.4). A 200 summary is
 * normal even when individual PR lookups failed; those arrive in `errors[]`
 * and are surfaced as separate danger toasts while the summary turns warning.
 * A 409 means another sync already holds the process-wide lock.
 */
export function SyncButton({ className }: SyncButtonProps) {
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const mutation = useMutation({
    mutationFn: () =>
      apiFetch<SyncResponse>("/sync", {
        method: "POST",
        body: JSON.stringify({}),
      }),
    onSuccess: (result) => {
      invalidateIssueViews(queryClient);
      invalidateSettingsViews(queryClient);
      const failed = result.errors.length;
      toast({
        title: "对账完成",
        description:
          `检查 ${result.checked} · 合并 ${result.merged} · 拒绝 ${result.rejected} · 仍未合并 ${result.still_open}` +
          (failed > 0 ? `；${failed} 个查询失败` : ""),
        variant: failed > 0 ? "warning" : "success",
      });
      for (const error of result.errors.slice(0, MAX_ERROR_TOASTS)) {
        toast({
          title: "对账查询失败",
          description: syncErrorText(error),
          variant: "danger",
          duration: ERROR_TOAST_MS,
        });
      }
    },
    onError: (error) => {
      const message =
        error instanceof ApiError && error.status === 409 ? SYNC_CONFLICT : describeError(error);
      toast({ title: "对账失败", description: message, variant: "danger" });
    },
  });

  return (
    <Button
      variant="secondary"
      size="sm"
      pending={mutation.isPending}
      onClick={() => mutation.mutate()}
      className={className}
    >
      <RefreshCw aria-hidden="true" className="size-3.5" />
      GitHub 对账
    </Button>
  );
}
