import { RefreshCw, RotateCcw } from "lucide-react";
import { useState } from "react";

import { Button, ConfirmDialog, useToast, type IssueState } from "../../components/ui";
import { ApiError } from "../../lib/api";
import { describeError } from "../overview/PanelChrome";
import { issueActionFor, useReopenIssue, useRequeueIssue } from "./actions";

interface IssueActionButtonProps {
  fingerprint: string;
  state: IssueState;
  stuck: boolean;
  size?: "sm" | "md";
  className?: string;
}

/** Error text plus any `fields` the server attached (e.g. a 409 state hint). */
function actionErrorDescription(error: unknown): string {
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
 * State-aware owner action (docs/09 §10.8): reopen a rejected/escalated issue
 * without a confirmation, or requeue a stuck issue behind a destructive
 * confirmation. Renders nothing when the state allows neither.
 */
export function IssueActionButton({
  fingerprint,
  state,
  stuck,
  size = "sm",
  className,
}: IssueActionButtonProps) {
  const action = issueActionFor({ state, stuck });
  const { toast } = useToast();
  const reopen = useReopenIssue(fingerprint);
  const requeue = useRequeueIssue(fingerprint);
  const [confirmOpen, setConfirmOpen] = useState(false);

  if (!action) {
    return null;
  }

  const reportFailure = (title: string, error: unknown) => {
    toast({
      title,
      description: actionErrorDescription(error),
      variant: "danger",
    });
  };

  if (action === "reopen") {
    return (
      <Button
        variant="secondary"
        size={size}
        className={className}
        pending={reopen.isPending}
        onClick={() => {
          reopen.mutate(undefined, {
            onSuccess: () => {
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
    );
  }

  return (
    <>
      <Button
        variant="secondary"
        size={size}
        className={className}
        pending={requeue.isPending}
        onClick={() => setConfirmOpen(true)}
      >
        <RefreshCw aria-hidden="true" className="size-4" />
        重排
      </Button>
      <ConfirmDialog
        open={confirmOpen}
        title="重排卡单问题？"
        description="仅当问题单处于卡单状态时有效。重排会清除归属并重新入队，可能重复执行同一问题的修复。"
        confirmLabel="确认重排"
        danger
        pending={requeue.isPending}
        onCancel={() => setConfirmOpen(false)}
        onConfirm={() => {
          requeue.mutate(undefined, {
            onSuccess: () => {
              setConfirmOpen(false);
              toast({
                title: "已重排",
                description: "问题单已清除归属并回到 new，等待 runner 重新领取。",
                variant: "success",
              });
            },
            onError: (error) => {
              // Close first: the toast region is inert while the modal is open.
              setConfirmOpen(false);
              reportFailure("重排失败", error);
            },
          });
        }}
      />
    </>
  );
}
