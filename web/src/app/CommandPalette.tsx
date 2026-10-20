import { useQuery } from "@tanstack/react-query";
import { ArrowRight, Search, X } from "lucide-react";
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
import { useProjectsQuery } from "../features/projects/queries";
import {
  MAX_SEARCH_QUERY_CHARS,
  type IssueListItem,
  type IssueListResponse,
} from "../features/issues/types";
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

/** Full-list path for the "查看全部结果" exit (docs/09 §12 "或列表筛选"). */
export function paletteListPath(term: string): string {
  const params = new URLSearchParams();
  params.set("q", normalizeSearchQuery(term));
  params.set("sort", "last_seen");
  params.set("order", "desc");
  return `/issues?${params.toString()}`;
}

/** Zero-query shortcuts (docs/09 §12 static commands). */
const QUICK_COMMANDS: readonly { label: string; hint: string; to: string }[] = [
  {
    label: "项目台账",
    hint: "按项目查看队列、运行器与交付质量",
    to: "/projects",
  },
  {
    label: "需要你处理",
    hint: "升级 / 卡单 / 被拒 / CI 超时",
    to: "/issues?attention=1",
  },
  { label: "卡单问题", hint: "疑似卡死的运行", to: "/issues?stuck=1" },
  { label: "运行器", hint: "runner 状态与重启", to: "/runners" },
  {
    label: "配置审计",
    hint: "设置变更与重启记录",
    to: "/events?kind=config_changed,config_migrated,restart_requested",
  },
];

type ClickHandler = (event: MouseEvent<HTMLAnchorElement>) => void;

interface EntryHandlers {
  /** Activates the entry from keyboard/click (quick commands). */
  onActivate: () => void;
  /** Modified-click-aware close behaviour (link entries). */
  onResultClick?: ClickHandler;
}

interface PaletteEntry {
  key: string;
  optionId: string;
  to: string;
  render: (handlers: EntryHandlers) => ReactNode;
}

export interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  /** Focus target restored on close; defaults to the previously focused node. */
  returnFocusRef?: RefObject<HTMLElement | null>;
}

/**
 * Global command palette (docs/09 §12): ⌘K / Ctrl+K from anywhere, type a
 * title or fingerprint, and jump straight to an issue. Arrow keys move the
 * selection, Enter activates it, and both a "查看全部结果" exit and zero-query
 * quick commands keep the palette from being a dead end. It is a portal
 * dialog with the shared modal behaviour (focus trap, Escape/backdrop
 * dismissal, background inert and focus restoration). The query clears on
 * every open and only fires once the trimmed term is at least one character
 * long.
 */
export function CommandPalette({ open, onClose, returnFocusRef }: CommandPaletteProps) {
  const navigate = useNavigate();
  const projects = useProjectsQuery();
  const panelRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [text, setText] = useState("");
  const [debounced, setDebounced] = useState("");
  const [wasOpen, setWasOpen] = useState(open);
  const [activeIndex, setActiveIndex] = useState(0);

  // Every open starts a fresh search; a previous term must never leak in.
  // Reset during render (rather than in an effect) so the reopened dialog
  // commits with an empty field instead of briefly painting the old term.
  if (open !== wasOpen) {
    setWasOpen(open);
    if (open) {
      setText("");
      setDebounced("");
      setActiveIndex(0);
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

  // Unified keyboard targets: quick commands on the empty query, otherwise
  // the matches plus a "view all" exit. Enter navigates to the highlighted
  // entry's `to`; clicks go through the rendered control.
  const issues = search.data?.data ?? [];
  // Projects match by name or identity and lead the results: the operator
  // usually thinks in projects, not fingerprints.
  const projectMatches = (projects.data?.data ?? [])
    .filter((project) => {
      const term = debounced.toLowerCase();
      if (!term) {
        return false;
      }
      return (
        project.repo.toLowerCase().includes(term) ||
        (project.name ?? "").toLowerCase().includes(term)
      );
    })
    .slice(0, 3);
  const entries: PaletteEntry[] =
    debounced.length === 0
      ? QUICK_COMMANDS.map((command) => ({
          key: command.to,
          optionId: `palette-option-${command.label}`,
          to: command.to,
          render: ({ onActivate }: EntryHandlers) => (
            <button
              type="button"
              onClick={onActivate}
              className="flex min-h-11 w-full flex-col justify-center gap-0.5 px-2.5 py-2 text-left"
            >
              <span className="text-body font-medium text-text">{command.label}</span>
              <span className="text-caption text-text-muted">{command.hint}</span>
            </button>
          ),
        }))
      : [
          ...projectMatches.map((project) => ({
            key: `project-${project.repo}`,
            optionId: `palette-option-project-${project.repo}`,
            to: `/projects/${encodeURIComponent(project.repo)}`,
            render: ({ onResultClick }: EntryHandlers) => (
              <Link
                to={`/projects/${encodeURIComponent(project.repo)}`}
                onClick={onResultClick}
                className="flex min-h-11 flex-col justify-center gap-0.5 px-2.5 py-2"
              >
                <span className="text-body font-medium text-text">
                  {project.name ?? project.repo}
                </span>
                <span className="font-mono text-caption text-text-muted">{project.repo}</span>
              </Link>
            ),
          })),
          ...issues.map((issue) => ({
            key: issue.fingerprint,
            optionId: `palette-option-issue-${issue.fingerprint}`,
            to: issueDetailPath(issue.fingerprint),
            render: ({ onResultClick }: EntryHandlers) => (
              <Link
                to={issueDetailPath(issue.fingerprint)}
                onClick={onResultClick}
                className="block"
              >
                <ResultRow issue={issue} />
              </Link>
            ),
          })),
          {
            key: "view-all",
            optionId: "palette-option-view-all",
            to: paletteListPath(debounced),
            render: ({ onResultClick }: EntryHandlers) => (
              <Link
                to={paletteListPath(debounced)}
                onClick={onResultClick}
                className="flex min-h-11 items-center justify-between gap-2 px-2.5 py-2 text-body-sm text-accent"
              >
                <span>在问题单中查看全部结果</span>
                <ArrowRight aria-hidden="true" className="size-3.5 shrink-0" />
              </Link>
            ),
          },
        ];

  const clampedIndex = Math.min(activeIndex, Math.max(0, entries.length - 1));
  const activeEntry = entries[clampedIndex];

  // Keep the highlighted row in view without jumping the page.
  const activeOptionId = activeEntry?.optionId;
  useEffect(() => {
    if (!open || !activeOptionId) {
      return;
    }
    // Optional call: jsdom (unit tests) does not implement scrollIntoView.
    document.getElementById(activeOptionId)?.scrollIntoView?.({ block: "nearest" });
  }, [open, activeOptionId]);

  if (!open) {
    return null;
  }

  const trimmed = text.trim();
  const waitingForDebounce = trimmed !== debounced;

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

  function activate(entry: PaletteEntry) {
    onClose();
    navigate(entry.to);
  }

  function handleInputKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing) {
      return;
    }
    if (event.key === "ArrowDown" && entries.length > 0) {
      event.preventDefault();
      setActiveIndex((clampedIndex + 1) % entries.length);
      return;
    }
    if (event.key === "ArrowUp" && entries.length > 0) {
      event.preventDefault();
      setActiveIndex((clampedIndex - 1 + entries.length) % entries.length);
      return;
    }
    // Enter activates the highlighted entry; with no movement that is the
    // first match (or first quick command). Never fire while a pending
    // debounce would make the visible row disagree with the term.
    if (
      event.key === "Enter" &&
      entries.length > 0 &&
      !waitingForDebounce &&
      !search.isPending
    ) {
      event.preventDefault();
      activate(entries[clampedIndex]);
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
      <div className="px-4 py-4">
        <p className="text-body-sm text-text-secondary">输入标题或指纹开始搜索。</p>
        <p className="mt-2 text-caption text-text-secondary">或直接前往：</p>
        <OptionList
          entries={entries}
          activeIndex={clampedIndex}
          onActivate={activate}
        />
      </div>
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
      <div className="px-4 py-6">
        <p className="text-body-sm text-text-secondary">没有匹配的问题单。</p>
        <Link
          to={paletteListPath(debounced)}
          onClick={handleResultClick}
          className="mt-2 inline-flex min-h-9 items-center gap-1 rounded-sm text-body-sm text-accent transition-colors hover:text-text pointer-fine:min-h-7"
        >
          在问题单中查看全部结果
          <ArrowRight aria-hidden="true" className="size-3.5" />
        </Link>
      </div>
    );
  } else {
    body = (
      <OptionList
        entries={entries}
        activeIndex={clampedIndex}
        onActivate={activate}
        onResultClick={handleResultClick}
      />
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
            aria-activedescendant={activeEntry?.optionId}
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
        <p className="border-t border-border px-4 py-1.5 text-caption text-text-muted">
          ↑↓ 选择 · Enter 打开 · Esc 关闭
        </p>
      </div>
    </div>,
    document.body,
  );
}

interface OptionListProps {
  entries: PaletteEntry[];
  activeIndex: number;
  onActivate: (entry: PaletteEntry) => void;
  /** Passed to link entries so modified clicks keep the palette open. */
  onResultClick?: ClickHandler;
}

/**
 * The keyboard-navigable option list. Each row carries `role="option"` and a
 * stable id; the input's `aria-activedescendant` points at the active one so
 * screen readers follow ↑↓ without focus leaving the field.
 */
function OptionList({ entries, activeIndex, onActivate, onResultClick }: OptionListProps) {
  return (
    <ul role="listbox" aria-label="搜索结果" className="max-h-[50vh] overflow-y-auto p-2">
      {entries.map((entry, index) => {
        const active = index === activeIndex;
        return (
          <li
            key={entry.key}
            role="option"
            id={entry.optionId}
            aria-selected={active}
            className={
              active
                ? "rounded-card bg-raised"
                : "rounded-card transition-colors duration-150 ease-out hover:bg-raised focus-within:bg-raised"
            }
          >
            {entry.render({ onActivate: () => onActivate(entry), onResultClick })}
          </li>
        );
      })}
    </ul>
  );
}

function ResultRow({ issue }: { issue: IssueListItem }) {
  return (
    <span className="flex min-h-11 flex-col justify-center gap-1 px-2.5 py-2">
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
    </span>
  );
}
