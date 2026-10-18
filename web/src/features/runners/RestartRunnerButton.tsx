import { useMutation, useQueryClient } from "@tanstack/react-query";
import { RotateCw } from "lucide-react";
import { useEffect, useId, useState } from "react";

import { Button, ConfirmDialog, useToast } from "../../components/ui";
import { ApiError, apiFetch } from "../../lib/api";
import { describeError } from "../../lib/errors";
import type { RunnerDto } from "./types";

/** A 501 explains the manual command; keep it on screen long enough to copy. */
const MANUAL_RESTART_TOAST_MS = 8000;

interface RestartRunnerButtonProps {
  runner: RunnerDto;
  className?: string;
}

function restartErrorDescription(error: unknown): string {
  if (error instanceof ApiError && error.status === 409) {
    return "已有重启任务在排程，请稍后重试。";
  }
  const message = describeError(error);
  if (error instanceof ApiError && error.fields) {
    const details = Object.entries(error.fields).map(
      ([field, detail]) => `${field}：${detail}`,
    );
    if (details.length > 0) {
      return `${message}（${details.join("；")}）`;
    }
  }
  return message;
}

/**
 * Restart the runner's systemd unit (docs/09 §9.2, §10.6). The endpoint only
 * schedules `systemd-run --on-active=2 systemctl restart <unit>`, so the toast
 * speaks of scheduling, not completion. Restarting one unit also restarts any
 * other runner registered under it, and interrupts the run in flight — the
 * confirmation spells both out before the POST.
 */
export function RestartRunnerButton({ runner, className }: RestartRunnerButtonProps) {
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [confirmOpen, setConfirmOpen] = useState(false);
  const disabledHintId = useId();
  const canRestart = Boolean(runner.service_unit);

  // A poll can clear `service_unit` (or the card can be reused for a runner
  // without one); an open confirmation must not outlive the capability.
  useEffect(() => {
    if (!canRestart) {
      setConfirmOpen(false);
    }
  }, [canRestart]);

  const mutation = useMutation({
    mutationFn: () =>
      apiFetch<void>(`/runners/${encodeURIComponent(runner.id)}/restart`, {
        method: "POST",
        body: JSON.stringify({}),
      }),
  });

  const unit = runner.service_unit ?? "—";
  const activeWarning = runner.current_fingerprint
    ? `当前正在修复 ${runner.current_fingerprint}，重启会中断该 run。`
    : "";
  const description =
    `服务单元：${unit}。${activeWarning}` +
    "同一 unit 承载的多个 runner 会一起重启；建议一 runner 一 unit。";

  return (
    <>
      <Button
        variant="secondary"
        size="sm"
        className={className}
        pending={mutation.isPending}
        disabled={!canRestart}
        title={canRestart ? undefined : "该 runner 未上报 service_unit，无法重启。"}
        aria-describedby={canRestart ? undefined : disabledHintId}
        aria-label={`重启 unit：${runner.id}`}
        onClick={() => setConfirmOpen(true)}
      >
        <RotateCw aria-hidden="true" className="size-4" />
        重启 unit
      </Button>
      {canRestart ? null : (
        // A disabled button swallows pointer events (and its title), so the
        // reason must be visible next to it.
        <span id={disabledHintId} className="text-caption text-text-secondary">
          未上报 service_unit
        </span>
      )}
      <ConfirmDialog
        open={confirmOpen}
        title="重启 runner unit？"
        description={description}
        confirmLabel="确认重启"
        danger
        pending={mutation.isPending}
        onCancel={() => setConfirmOpen(false)}
        onConfirm={() => {
          mutation.mutate(undefined, {
            onSuccess: () => {
              setConfirmOpen(false);
              void queryClient.invalidateQueries({ queryKey: ["runners"] });
              void queryClient.invalidateQueries({ queryKey: ["overview"] });
              void queryClient.invalidateQueries({ queryKey: ["events"] });
              toast({
                title: "重启已排程",
                description: `systemd 将在约 2 秒后重启 ${unit}，期间该 runner 会短暂离线。`,
                variant: "success",
              });
            },
            onError: (error) => {
              // Close first: the toast region is inert while the modal is open.
              setConfirmOpen(false);
              if (error instanceof ApiError && error.status === 501) {
                toast({
                  title: "无法排程重启",
                  description: error.message,
                  variant: "danger",
                  duration: MANUAL_RESTART_TOAST_MS,
                });
                return;
              }
              toast({
                title: "重启失败",
                description: restartErrorDescription(error),
                variant: "danger",
              });
            },
          });
        }}
      />
    </>
  );
}
