import { clsx } from "clsx";
import { EyeOff, RefreshCw, RotateCcw } from "lucide-react";
import { useEffect, useState } from "react";

import { Button, ConfirmDialog, Input, useToast, type IssueState } from "../../components/ui";
import { ApiError } from "../../lib/api";
import { describeError } from "../../lib/errors";
import {
  canIgnore,
  canReopen,
  canRequeue,
  useIgnoreIssue,
  useReopenIssue,
  useRequeueIssue,
} from "./actions";
import { shortFingerprint } from "./format";

interface IssueActionButtonProps {
  fingerprint: string;
  state: IssueState;
  stuck: boolean;
  size?: "sm" | "md";
  className?: string;
}

type OwnerDialog = "ignore" | "requeue" | null;

/** How long a successful write keeps the buttons disabled while views refetch. */
const COOLDOWN_MS = 3000;

/**
 * Error text plus any `fields` the server attached (e.g. a 409 state hint).
 * The server's `invalid_state` body is English ("state is fixing, not …");
 * map the known code to operator language while keeping the field detail,
 * instead of leaking the raw message into the zh-CN UI.
 */
function actionErrorDescription(error: unknown): string {
  if (error instanceof ApiError && error.code === "invalid_state") {
    const base = "问题单状态已变化（可能已被其他操作处理），请刷新后重试";
    const details = error.fields
      ? Object.entries(error.fields).map(([field, detail]) => `${field}：${detail}`)
      : [];
    return details.length > 0 ? `${base}（${details.join("；")}）` : `${base}。`;
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
 * State-aware owner actions (docs/09 §8.5): reopen (with an optional review
 * note), mute (ignore), or requeue a stuck run. Every dialog is explicit; the
 * note is persisted and fed into the next repair prompt.
 */
export function IssueActionButton({
  fingerprint,
  state,
  stuck,
  size = "sm",
  className,
}: IssueActionButtonProps) {
  const showReopen = canReopen(state);
  const showIgnore = canIgnore(state);
  const showRequeue = canRequeue(state, stuck);
  const { toast } = useToast();
  const reopen = useReopenIssue(fingerprint);
  const ignore = useIgnoreIssue(fingerprint);
  const requeue = useRequeueIssue(fingerprint);
  const [dialog, setDialog] = useState<OwnerDialog>(null);
  const [reason, setReason] = useState("");
  const [cooldown, setCooldown] = useState(false);

  // A poll can make an action disappear and reappear; never keep a stale
  // dialog for a transition the server would now reject.
  useEffect(() => {
    if (
      (dialog === "ignore" && !showIgnore) ||
      (dialog === "requeue" && !showRequeue)
    ) {
      setDialog(null);
      setReason("");
    }
  }, [dialog, showIgnore, showRequeue]);

  // Reset the latch and note on every identity change so a recycled row does
  // not inherit the previous issue's pending dialog.
  useEffect(() => {
    setCooldown(false);
    setDialog(null);
    setReason("");
  }, [fingerprint, state, stuck]);

  useEffect(() => {
    if (!cooldown) {
      return;
    }
    const timer = window.setTimeout(() => setCooldown(false), COOLDOWN_MS);
    return () => window.clearTimeout(timer);
  }, [cooldown]);

  if (!showReopen && !showIgnore && !showRequeue) {
    return null;
  }

  const shortFp = shortFingerprint(fingerprint);
  const pending = reopen.isPending || ignore.isPending || requeue.isPending;
  const disabled = cooldown || pending;

  const reportFailure = (title: string, error: unknown) => {
    toast({
      title,
      description: actionErrorDescription(error),
      variant: "danger",
    });
  };

  const close = () => {
    setDialog(null);
    setReason("");
  };

  return (
    <>
      <div className={clsx("flex flex-wrap items-center justify-end gap-1", className)}>
        {showReopen ? (
          <Button
            variant="secondary"
            size={size}
            className="whitespace-nowrap"
            pending={reopen.isPending}
            disabled={cooldown}
            aria-label={`重新打开：${shortFp}`}
            onClick={() => {
              reopen.mutate(undefined, {
                onSuccess: () => {
                  setCooldown(true);
                  toast({
                    title: "已重新入队",
                    description: "问题单已回到 new，等待 runner 领取。",
                    variant: "success",
                  });
                },
                onError: (error) => reportFailure("重新打开失败", error),
              });
            }}
          >
            <RotateCcw aria-hidden="true" className="size-4" />
            重新打开
          </Button>
        ) : null}
        {showIgnore ? (
          <Button
            variant="secondary"
            size={size}
            className="whitespace-nowrap"
            disabled={disabled}
            aria-label={`忽略：${shortFp}`}
            onClick={() => setDialog("ignore")}
          >
            <EyeOff aria-hidden="true" className="size-4" />
            忽略
          </Button>
        ) : null}
        {showRequeue ? (
          <Button
            variant="secondary"
            size={size}
            className="whitespace-nowrap"
            disabled={disabled}
            aria-label={`重排：${shortFp}`}
            onClick={() => setDialog("requeue")}
          >
            <RefreshCw aria-hidden="true" className="size-4" />
            重排
          </Button>
        ) : null}
      </div>

      <ConfirmDialog
        open={dialog === "ignore"}
        title="忽略问题？"
        description="忽略后同指纹报告只计数，不再自动修复；只有手动重新打开能恢复。"
        confirmLabel="确认忽略"
        danger
        pending={ignore.isPending}
        onCancel={close}
        onConfirm={() => {
          ignore.mutate(reason, {
            onSuccess: () => {
              close();
              setCooldown(true);
              toast({
                title: "已忽略",
                description: "同指纹报告将被计数，不再自动修复。",
                variant: "success",
              });
            },
            onError: (error) => reportFailure("忽略失败", error),
          });
        }}
      >
        <Input
          label="忽略原因（可选）"
          value={reason}
          maxLength={2000}
          placeholder="例如：预期行为 / 第三方噪音"
          onChange={(event) => setReason(event.target.value)}
        />
      </ConfirmDialog>

      <ConfirmDialog
        open={dialog === "requeue"}
        title="重排卡单问题？"
        description="仅当问题单处于卡单状态时有效。重排会清除归属并重新入队，可能重复执行同一问题的修复。"
        confirmLabel="确认重排"
        danger
        pending={requeue.isPending}
        onCancel={close}
        onConfirm={() => {
          requeue.mutate(undefined, {
            onSuccess: () => {
              close();
              setCooldown(true);
              toast({
                title: "已重排",
                description: "问题单已清除归属并回到 new，等待 runner 重新领取。",
                variant: "success",
              });
            },
            onError: (error) => {
              close();
              reportFailure("重排失败", error);
            },
          });
        }}
      />
    </>
  );
}
