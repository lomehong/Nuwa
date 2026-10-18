import { useQuery } from "@tanstack/react-query";
import { Search, X } from "lucide-react";
import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { Link, useNavigate } from "react-router-dom";

import { Input, SeverityStone, Skeleton, StateBadge } from "../components/ui";
import { normalizeSearchQuery } from "../features/issues/filters";
import { issueDetailPath, shortFingerprint } from "../features/issues/format";
import { MAX_SEARCH_QUERY_CHARS, type IssueListResponse } from "../features/issues/types";
import { apiFetch, ApiError } from "../lib/api";
import { useModalBehavior } from "../lib/modal";

/** docs/09 §12: type-ahead debounce for the global search. */
export const PALETTE_DEBOUNCE_MS = 150;
export const PALETTE_RESULT_LIMIT = 5;

/** `GET /issues` path for the palette (newest activity first, five rows). */
export function paletteSearchPath(term: string): string {
  const params = new URLSearchParams();
  params.set("q", normalizeSearchQuery(term));
  params.set("page_size", String(PALETTE_RESULT_LIMIT));
  params.set("sort", "last_seen");
  params.set("order", "desc");
  return `/issues?${params.toString()}`;
}

export interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  /** Focus target restored on close; defaults to the previously focused node. */
  returnFocusRef?: RefObject<HTMLElement | null>;
}

/**
 * Global command palette (docs/09 §12): ⌘K / Ctrl+K from anywhere, type a
 * title or fingerprint, and jump straight to an issue. It is a portal dialog
 * with the shared modal behaviour (focus trap, Escape/backdrop dismissal,
 * background inert and focus restoration). The query clears on every open and
 * only fires once the trimmed term is at least one character long.
 */
export function CommandPalette({ open, onClose, returnFocusRef }: CommandPaletteProps) {
  const navigate = useNavigate();
  const panelRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [text, setText] = useState("");
  const [debounced, setDebounced] = useState("");
  const [wasOpen, setWasOpen] = useState(open);

  // Every open starts a fresh search; a previous term must never leak in.
  // Reset during render (rather than in an effect) so the reopened dialog
  // commits with an empty field instead of briefly painting the old term.
  if (open !== wasOpen) {
    setWasOpen(open);
    if (open) {
      setText("");
      setDebounced("");
    }
  }

  const handleKeyDown = useModalBehavior({
    active: open,
    containerRef: panelRef,
    initialFocusRef: inputRef,
    returnFocusRef,
    onEscape: onClose,
  });

  useEffect(() => {
    if (!open) {
      return;
    }
    const handle = window.setTimeout(() => setDebounced(text.trim()), PALETTE_DEBOUNCE_MS);
    return () => window.clearTimeout(handle);
  }, [open, text]);

  const search = useQuery({
    queryKey: ["issues", "palette", debounced],
    queryFn: ({ signal }) =>
      apiFetch<IssueListResponse>(paletteSearchPath(debounced), { signal }),
    enabled: open && debounced.length >= 1,
    staleTime: 15_000,
    retry: false,
  });

  if (!open) {
    return null;
  }

  const trimmed = text.trim();
  const waitingForDebounce = trimmed !== debounced;
  const issues = search.data?.data ?? [];
  const firstResult = issues[0];

  // Announce the settled result count (or emptiness) through a persistent
  // live region: a status node that mounts with its text is often not read.
  const settled =
    trimmed.length > 0 &&
    !waitingForDebounce &&
    !search.isPending &&
    !search.isError;
  const announcement = !settled
    ? ""
    : issues.length === 0
      ? "没有匹配的问题单"
      : `找到 ${issues.length} 条问题单`;

  function handleInputKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    // Enter jumps to the newest match, but never while an IME composition or
    // a pending debounce would make the visible row disagree with the term.
    if (
      event.key === "Enter" &&
      firstResult &&
      !waitingForDebounce &&
      !search.isPending &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      onClose();
      navigate(issueDetailPath(firstResult.fingerprint));
    }
  }

  /**
   * Only an unmodified primary click navigates inside the SPA. A modified
   * click (Ctrl/Cmd/Shift/Alt, middle button) means "open in a new tab", so
   * the palette stays mounted; otherwise closing it would tear down the page
   * the new tab was opened from.
   */
  function handleResultClick(event: MouseEvent<HTMLAnchorElement>) {
    const unmodifiedPrimary =
      event.button === 0 &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.shiftKey &&
      !event.altKey;
    if (unmodifiedPrimary) {
      onClose();
    }
  }

  let body: ReactNode;
  if (trimmed.length === 0) {
    body = (
      <p className="px-4 py-8 text-body-sm text-text-secondary">
        输入标题或指纹开始搜索。
      </p>
    );
  } else if (waitingForDebounce || search.isPending) {
    body = (
      <div role="status" aria-busy="true" className="space-y-2 p-3">
        <span className="sr-only">正在搜索问题单</span>
        {Array.from({ length: 3 }, (_, index) => (
          <div key={index} className="flex items-center gap-3 px-2 py-2">
            <Skeleton className="h-4 w-1/2" />
            <Skeleton rounded="full" className="ml-auto h-4 w-20" />
          </div>
        ))}
      </div>
    );
  } else if (search.isError) {
    const message =
      search.error instanceof ApiError
        ? search.error.message || "请求失败"
        : "无法联系服务器，请稍后重试。";
    body = (
      <div className="flex flex-wrap items-start justify-between gap-2 p-4">
        <div role="alert" className="min-w-0 flex-1">
          <p className="text-body font-medium text-text">搜索失败</p>
          <p className="mt-0.5 text-body-sm text-text-secondary">{message}</p>
        </div>
        <button
          type="button"
          onClick={() => void search.refetch()}
          className="inline-flex min-h-11 shrink-0 items-center rounded-card border border-border-strong bg-raised px-3 text-caption font-medium text-text transition-colors hover:border-accent hover:text-accent"
        >
          重试
        </button>
      </div>
    );
  } else if (issues.length === 0) {
    body = (
      <p className="px-4 py-8 text-body-sm text-text-secondary">没有匹配的问题单</p>
    );
  } else {
    body = (
      <ul aria-label="搜索结果" className="max-h-[50vh] overflow-y-auto p-2">
        {issues.map((issue) => (
          <li key={issue.fingerprint}>
            <Link
              to={issueDetailPath(issue.fingerprint)}
              onClick={handleResultClick}
              className="flex min-h-11 flex-col justify-center gap-1 rounded-card px-2.5 py-2 transition-colors duration-150 ease-out hover:bg-raised focus-visible:bg-raised"
            >
              <span className="truncate text-body font-medium text-text">
                {issue.title || "（无标题）"}
              </span>
              <span className="flex flex-wrap items-center gap-2">
                <span className="font-mono text-caption text-text-muted">
                  {shortFingerprint(issue.fingerprint)}
                </span>
                <SeverityStone severity={issue.severity} />
                <StateBadge state={issue.state} />
              </span>
            </Link>
          </li>
        ))}
      </ul>
    );
  }

  return createPortal(
    <div
      role="presentation"
      data-testid="command-palette-backdrop"
      className="nuwa-backdrop fixed inset-0 z-50 flex items-start justify-center bg-scrim p-4 pt-[12vh]"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onClose();
        }
      }}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label="搜索问题单"
        tabIndex={-1}
        onKeyDown={handleKeyDown}
        className="nuwa-dialog w-full max-w-xl overflow-hidden rounded-modal border border-border-strong bg-surface text-text shadow-overlay"
      >
        <div className="flex items-center gap-2 border-b border-border px-4 py-2">
          <Search aria-hidden="true" className="size-4 shrink-0 text-text-muted" />
          <Input
            ref={inputRef}
            label="搜索问题单"
            hideLabel
            type="search"
            value={text}
            autoFocus
            maxLength={MAX_SEARCH_QUERY_CHARS}
            placeholder="标题或指纹，例如 panic"
            onChange={(event) => setText(event.target.value)}
            onKeyDown={handleInputKeyDown}
            className="border-0 bg-transparent focus-visible:border-transparent"
          />
          <button
            type="button"
            aria-label="关闭搜索"
            onClick={onClose}
            className="flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text"
          >
            <X aria-hidden="true" className="size-4" />
          </button>
        </div>
        <p role="status" className="sr-only">
          {announcement}
        </p>
        {body}
      </div>
    </div>,
    document.body,
  );
}
