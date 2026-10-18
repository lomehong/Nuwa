import { clsx } from "clsx";
import { X } from "lucide-react";
import { useId, useRef } from "react";
import { createPortal } from "react-dom";

import { useModalBehavior } from "../../lib/modal";
import { Button } from "./Button";

export interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  pending?: boolean;
  /** Locks the cancel button while the confirm action is in flight. */
  cancelDisabled?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Modal confirmation with a focus trap, Escape and backdrop dismissal, focus
 * restoration to the invoking element, background `inert` and body scroll lock
 * (docs/09 §11.6). Rendered through a portal so the rest of the document can
 * be inert while the dialog is open.
 */
export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel = "确认",
  cancelLabel = "取消",
  danger = false,
  pending = false,
  cancelDisabled = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const titleId = useId();
  const descriptionId = useId();
  const handleKeyDown = useModalBehavior({
    active: open,
    containerRef: dialogRef,
    initialFocusRef: cancelRef,
    onEscape: onCancel,
  });

  if (!open) {
    return null;
  }

  return createPortal(
    <div
      role="presentation"
      className="nuwa-backdrop fixed inset-0 z-50 flex items-end justify-center bg-scrim p-4 sm:items-center"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onCancel();
        }
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descriptionId : undefined}
        tabIndex={-1}
        onKeyDown={handleKeyDown}
        className="nuwa-dialog w-full max-w-md rounded-modal border border-border-strong bg-surface p-5 text-text shadow-overlay"
      >
        <div className="flex items-start justify-between gap-4">
          <h2 id={titleId} className="text-heading-sm font-semibold">
            {title}
          </h2>
          <button
            type="button"
            aria-label="关闭对话框"
            onClick={onCancel}
            className="-mr-2 -mt-2 flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text"
          >
            <X aria-hidden="true" className="size-4" />
          </button>
        </div>
        {description ? (
          <p id={descriptionId} className="mt-2 break-words text-body-sm text-text-secondary">
            {description}
          </p>
        ) : null}
        <div className="mt-5 flex flex-wrap justify-end gap-2">
          <Button ref={cancelRef} variant="secondary" disabled={cancelDisabled} onClick={onCancel}>
            {cancelLabel}
          </Button>
          <Button
            variant={danger ? "danger" : "primary"}
            pending={pending}
            onClick={onConfirm}
            className={clsx(pending && "cursor-progress")}
          >
            {confirmLabel}
          </Button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
