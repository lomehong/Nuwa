import { clsx } from "clsx";
import { AlertTriangle, CheckCircle2, Info, X, XCircle, type LucideIcon } from "lucide-react";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FocusEvent,
  type ReactNode,
} from "react";

export type ToastVariant = "info" | "success" | "warning" | "danger";

export interface ToastOptions {
  title: string;
  description?: string;
  variant?: ToastVariant;
  /** Milliseconds before auto-dismiss; `0` keeps the toast until dismissed. */
  duration?: number;
}

interface ToastRecord extends ToastOptions {
  id: number;
  variant: ToastVariant;
  duration: number;
}

interface ToastContextValue {
  toast: (options: ToastOptions) => number;
  dismiss: (id: number) => void;
}

/** Auto-dismiss pauses independently for pointer hover, focus and a hidden tab. */
type PauseReason = "pointer" | "focus" | "visibility";

interface TimerRecord {
  handle: number | null;
  remaining: number;
  startedAt: number;
  paused: Set<PauseReason>;
}

const ToastContext = createContext<ToastContextValue | null>(null);

const VARIANT_ICONS: Record<ToastVariant, LucideIcon> = {
  info: Info,
  success: CheckCircle2,
  warning: AlertTriangle,
  danger: XCircle,
};

const VARIANT_ICON_CLASSES: Record<ToastVariant, string> = {
  info: "text-info",
  success: "text-success",
  warning: "text-warning",
  danger: "text-danger",
};

const DEFAULT_DURATION = 5000;

function isInside(event: FocusEvent<HTMLElement>): boolean {
  const next = event.relatedTarget;
  return next instanceof Node && event.currentTarget.contains(next);
}

/**
 * Toast provider. Both live regions mount up front (empty) so assistive
 * technology reliably announces additions: informational toasts are polite,
 * danger toasts (failed writes) announce assertively.
 *
 * Auto-dismiss (WCAG 2.2.1) stops while the pointer is over the toast, while
 * focus is inside it, and while the tab is hidden, then resumes with the
 * remaining time. `duration: 0` toasts stay until explicitly closed.
 */
export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastRecord[]>([]);
  const nextId = useRef(0);
  const timers = useRef(new Map<number, TimerRecord>());

  const dismiss = useCallback((id: number) => {
    const timer = timers.current.get(id);
    if (timer && timer.handle !== null) {
      window.clearTimeout(timer.handle);
    }
    timers.current.delete(id);
    setToasts((current) => current.filter((item) => item.id !== id));
  }, []);

  const pause = useCallback((id: number, reason: PauseReason) => {
    const timer = timers.current.get(id);
    if (!timer) {
      return;
    }
    timer.paused.add(reason);
    if (timer.handle === null) {
      return;
    }
    window.clearTimeout(timer.handle);
    timer.handle = null;
    timer.remaining = Math.max(0, timer.remaining - (Date.now() - timer.startedAt));
  }, []);

  const resume = useCallback(
    (id: number, reason: PauseReason) => {
      const timer = timers.current.get(id);
      if (!timer) {
        return;
      }
      timer.paused.delete(reason);
      if (timer.paused.size > 0 || timer.handle !== null || timer.remaining <= 0) {
        return;
      }
      timer.startedAt = Date.now();
      timer.handle = window.setTimeout(() => dismiss(id), timer.remaining);
    },
    [dismiss],
  );

  const toast = useCallback(
    (options: ToastOptions) => {
      const id = ++nextId.current;
      const record: ToastRecord = {
        ...options,
        id,
        variant: options.variant ?? "info",
        duration: options.duration ?? DEFAULT_DURATION,
      };

      setToasts((current) => [...current, record]);

      if (record.duration > 0) {
        const paused = new Set<PauseReason>();
        if (document.hidden) {
          paused.add("visibility");
        }
        const timer: TimerRecord = {
          handle: null,
          remaining: record.duration,
          startedAt: Date.now(),
          paused,
        };
        timers.current.set(id, timer);
        if (paused.size === 0) {
          timer.handle = window.setTimeout(() => dismiss(id), record.duration);
        }
      }

      return id;
    },
    [dismiss],
  );

  useEffect(() => {
    function handleVisibilityChange() {
      for (const id of timers.current.keys()) {
        if (document.hidden) {
          pause(id, "visibility");
        } else {
          resume(id, "visibility");
        }
      }
    }

    document.addEventListener("visibilitychange", handleVisibilityChange);
    return () => document.removeEventListener("visibilitychange", handleVisibilityChange);
  }, [pause, resume]);

  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending.values()) {
        if (timer.handle !== null) {
          window.clearTimeout(timer.handle);
        }
      }
      pending.clear();
    };
  }, []);

  const value = useMemo(() => ({ toast, dismiss }), [toast, dismiss]);

  // Danger toasts (failed writes) announce assertively; informational ones
  // stay polite. Both regions mount up front (empty) so assistive technology
  // reliably announces additions; `display: contents` keeps one visual column.
  const urgent = toasts.filter((item) => item.variant === "danger");
  const calm = toasts.filter((item) => item.variant !== "danger");

  const renderToast = (item: ToastRecord) => {
    const Icon = VARIANT_ICONS[item.variant];
    return (
      <div
        key={item.id}
        data-toast={item.id}
        onPointerEnter={() => pause(item.id, "pointer")}
        onPointerLeave={() => resume(item.id, "pointer")}
        onFocus={(event) => {
          if (!isInside(event)) {
            pause(item.id, "focus");
          }
        }}
        onBlur={(event) => {
          if (!isInside(event)) {
            resume(item.id, "focus");
          }
        }}
        className="nuwa-toast pointer-events-auto flex w-full max-w-sm items-start gap-3 rounded-modal border border-border-strong bg-raised p-3 text-text shadow-overlay"
      >
        <Icon
          aria-hidden="true"
          className={clsx("mt-0.5 size-4 shrink-0", VARIANT_ICON_CLASSES[item.variant])}
        />
        <div className="min-w-0 flex-1">
          <p className="text-body-sm font-medium">{item.title}</p>
          {item.description ? (
            <p className="mt-0.5 break-words text-caption text-text-secondary">
              {item.description}
            </p>
          ) : null}
        </div>
        <button
          type="button"
          aria-label={`关闭通知：${item.title}`}
          onClick={() => dismiss(item.id)}
          className="-mr-1 -mt-1 flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-surface hover:text-text"
        >
          <X aria-hidden="true" className="size-4" />
        </button>
      </div>
    );
  };

  return (
    <ToastContext.Provider value={value}>
      {children}
      <div
        role="presentation"
        className="pointer-events-none fixed inset-x-0 bottom-0 z-50 flex flex-col items-center gap-2 p-4 sm:items-end sm:p-6"
      >
        {/* No `role="alert"` here: a persistent alert region would collide with
            every page's own findByRole("alert") error panels. Explicit
            aria-live carries the assertive announcement on its own. */}
        <div className="contents" aria-live="assertive" aria-atomic="false">
          {urgent.map(renderToast)}
        </div>
        <div
          className="contents"
          role="status"
          aria-live="polite"
          aria-atomic="false"
          aria-label="通知"
        >
          {calm.map(renderToast)}
        </div>
      </div>
    </ToastContext.Provider>
  );
}

export function useToast(): ToastContextValue {
  const context = useContext(ToastContext);
  if (!context) {
    throw new Error("useToast must be used inside a <ToastProvider>");
  }
  return context;
}
