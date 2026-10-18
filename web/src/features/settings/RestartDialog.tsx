import { clsx } from "clsx";
import { Loader2, X } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Link } from "react-router-dom";

import { Button, ConfirmDialog } from "../../components/ui";
import { ApiError } from "../../lib/api";
import { describeError } from "../../lib/errors";
import { waitForServeRecovery, reloadConsole } from "../../lib/health";
import { useModalBehavior } from "../../lib/modal";
import { formatAge } from "../overview/format";
import { useRestartServicesMutation } from "./queries";
import type { ActiveRunSummary } from "./types";

type Phase = "confirm" | "active" | "restarting" | "failed";

export interface RestartDialogProps {
  open: boolean;
  onClose: () => void;
  /** systemd units the server will restart, from `/system.services`. */
  units: string[];
}

function isActiveRun(value: unknown): value is ActiveRunSummary {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const run = value as Partial<ActiveRunSummary>;
  return (
    typeof run.id === "string" &&
    typeof run.fingerprint === "string" &&
    typeof run.runner_id === "string" &&
    typeof run.phase === "string" &&
    typeof run.started_at_ms === "number"
  );
}

/** The 409 guard list; a malformed body degrades to the error message. */
function activeRunsFrom(body: unknown): ActiveRunSummary[] {
  if (typeof body !== "object" || body === null) {
    return [];
  }
  const runs = (body as { active_runs?: unknown }).active_runs;
  return Array.isArray(runs) ? runs.filter(isActiveRun) : [];
}

/**
 * Guarded restart flow (docs/09 §10.8, §13.3): confirm, surface the active
 * runs a 409 lists, require an explicit acknowledgement before forcing, then
 * poll `/health` and `/auth/me` and reload once the new process is serving.
 * A 501 means systemd-run is unavailable; its message carries the manual
 * command and is shown verbatim.
 */
export function RestartDialog({ open, onClose, units }: RestartDialogProps) {
  const restart = useRestartServicesMutation();
  const [phase, setPhase] = useState<Phase>("confirm");
  const [activeRuns, setActiveRuns] = useState<ActiveRunSummary[]>([]);
  const [acknowledged, setAcknowledged] = useState(false);
  const [forced, setForced] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const attemptRef = useRef(0);
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descriptionId = useId();
  const unitsLabel = units.length > 0 ? units.join("、") : "未配置 systemd unit";

  // A fresh open always starts at the confirmation step, dropping any runs
  // from a previous attempt.
  useEffect(() => {
    if (open) {
      setPhase("confirm");
      setActiveRuns([]);
      setAcknowledged(false);
      setForced(false);
      setMessage(null);
    }
  }, [open]);

  const settle = useCallback(() => {
    attemptRef.current += 1;
  }, []);

  const handleClose = useCallback(() => {
    if (phase === "restarting") {
      return;
    }
    settle();
    // Reset before the parent hides the dialog so a reopen cannot flash the
    // previous attempt's guard or error state.
    setPhase("confirm");
    setActiveRuns([]);
    setAcknowledged(false);
    setMessage(null);
    onClose();
  }, [onClose, phase, settle]);

  const runRecovery = useCallback(
    async (attempt: number) => {
      try {
        await waitForServeRecovery();
        if (attempt === attemptRef.current) {
          reloadConsole();
        }
      } catch {
        if (attempt !== attemptRef.current) {
          return;
        }
        setMessage(
          `服务在 60 秒内未恢复，请检查 systemd 状态：systemctl status ${units.join(" ")}`,
        );
        setPhase("failed");
      }
    },
    [units],
  );

  const submit = useCallback(
    (force: boolean) => {
      const attempt = attemptRef.current + 1;
      attemptRef.current = attempt;
      setForced(force);
      restart.mutate(
        { force },
        {
          onSuccess: () => {
            if (attempt !== attemptRef.current) {
              return;
            }
            setPhase("restarting");
            void runRecovery(attempt);
          },
          onError: (error) => {
            if (attempt !== attemptRef.current) {
              return;
            }
            if (error instanceof ApiError && error.status === 409) {
              const runs = activeRunsFrom(error.body);
              if (runs.length > 0) {
                setActiveRuns(runs);
                setAcknowledged(false);
                setPhase("active");
                return;
              }
            }
            setMessage(describeError(error));
            setPhase("failed");
          },
        },
      );
    },
    [restart, runRecovery],
  );

  const statusOpen = open && phase !== "confirm";
  const handleKeyDown = useModalBehavior({
    active: statusOpen,
    containerRef: dialogRef,
    // Escape cannot cancel a restart already handed to systemd.
    onEscape: phase === "restarting" ? undefined : handleClose,
  });

  if (!open) {
    return null;
  }

  if (phase === "confirm") {
    return (
      <ConfirmDialog
        open
        title="重启服务？"
        description={`将重启：${unitsLabel}。正在进行的修复会被中断。`}
        confirmLabel="确认重启"
        cancelLabel="取消"
        danger
        pending={restart.isPending}
        onConfirm={() => submit(false)}
        onCancel={handleClose}
      />
    );
  }

  const status =
    phase === "restarting" ? (
      <p role="status" className="mt-4 flex items-center gap-2 text-body-sm text-text">
        <Loader2 aria-hidden="true" className="size-4 animate-spin text-accent" />
        正在重启，等待服务恢复…
      </p>
    ) : null;

  return createPortal(
    <div
      role="presentation"
      className="nuwa-backdrop fixed inset-0 z-50 flex items-end justify-center bg-scrim p-4 sm:items-center"
      onMouseDown={(event) => {
        if (phase !== "restarting" && event.target === event.currentTarget) {
          handleClose();
        }
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={status ? undefined : descriptionId}
        tabIndex={-1}
        onKeyDown={handleKeyDown}
        className="nuwa-dialog w-full max-w-lg rounded-modal border border-border-strong bg-surface p-5 text-text shadow-overlay"
      >
        <div className="flex items-start justify-between gap-4">
          <h2 id={titleId} className="text-heading-sm font-semibold">
            {phase === "active" ? "有正在进行的修复" : phase === "failed" ? "重启未完成" : "重启服务"}
          </h2>
          {phase !== "restarting" ? (
            <button
              type="button"
              aria-label="关闭对话框"
              onClick={handleClose}
              className="-mr-2 -mt-2 flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text"
            >
              <X aria-hidden="true" className="size-4" />
            </button>
          ) : null}
        </div>

        {phase === "active" ? (
          <>
            <p id={descriptionId} className="mt-2 text-body-sm text-text-secondary">
              强制重启会中断下列 {activeRuns.length} 个 run：
            </p>
            <ul className="mt-3 divide-y divide-border rounded-card border border-border">
              {activeRuns.map((run) => (
                <li
                  key={run.id}
                  className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2"
                >
                  <Link
                    to={`/issues/${encodeURIComponent(run.fingerprint)}`}
                    className="font-mono text-caption break-all text-accent underline-offset-2 hover:underline"
                  >
                    {run.fingerprint}
                  </Link>
                  <span className="font-mono text-caption text-text-secondary">
                    {run.runner_id}
                  </span>
                  <span className="text-caption text-text-secondary">{run.phase}</span>
                  <span className="text-caption text-text-muted">
                    {formatAge(run.started_at_ms)}
                  </span>
                </li>
              ))}
            </ul>
            <label className="mt-4 flex items-start gap-2 text-body-sm text-text">
              <input
                type="checkbox"
                checked={acknowledged}
                onChange={(event) => setAcknowledged(event.target.checked)}
                className="mt-0.5 size-4 accent-danger"
              />
              <span>我了解这会中断正在进行的修复</span>
            </label>
            <div className="mt-5 flex flex-wrap justify-end gap-2">
              <Button variant="secondary" onClick={handleClose}>
                取消
              </Button>
              <Button
                variant="danger"
                disabled={!acknowledged}
                pending={restart.isPending}
                onClick={() => submit(true)}
              >
                强制重启
              </Button>
            </div>
          </>
        ) : null}

        {status}

        {phase === "failed" ? (
          <>
            <p
              id={descriptionId}
              role="alert"
              className={clsx(
                "mt-3 rounded-card border border-danger/40 bg-danger/10 px-3 py-2",
                "text-body-sm break-words text-text",
              )}
            >
              {message}
            </p>
            <div className="mt-5 flex flex-wrap justify-end gap-2">
              <Button variant="secondary" onClick={handleClose}>
                关闭
              </Button>
              <Button variant="danger" pending={restart.isPending} onClick={() => submit(forced)}>
                重试
              </Button>
            </div>
          </>
        ) : null}
      </div>
    </div>,
    document.body,
  );
}
