import { clsx } from "clsx";
import { AlertTriangle, Check, Clock3, Eraser } from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";

import {
  Button,
  Input,
  ISSUE_STATE_VALUES,
  issueStateLabel,
  Select,
  severityLabel,
  type IssueState,
} from "../../components/ui";
import {
  canonicalStates,
  hasActiveFilters,
  isFilterSeverity,
  isIssueOrder,
  isIssueSort,
} from "./filters";
import {
  DEFAULT_PAGE_SIZE,
  ISSUE_ORDER_LABELS,
  ISSUE_ORDER_VALUES,
  ISSUE_SORT_LABELS,
  ISSUE_SORT_VALUES,
  MAX_PAGE_SIZE,
  PAGE_SIZE_OPTIONS,
  SEVERITY_FILTER_VALUES,
  type IssueListFilters,
} from "./types";

export const FILTER_DEBOUNCE_MS = 300;

const CHIP_BASE =
  "inline-flex min-h-11 items-center gap-1.5 rounded-full border px-3 text-caption font-medium " +
  "transition-colors duration-150 ease-out";
const CHIP_ACTIVE = "border-accent bg-accent/10 text-text";
const CHIP_IDLE =
  "border-border bg-surface text-text-secondary hover:border-border-strong hover:text-text";

function chipClass(active: boolean): string {
  return clsx(CHIP_BASE, active ? CHIP_ACTIVE : CHIP_IDLE);
}

function toggleState(states: IssueState[], state: IssueState): IssueState[] {
  return states.includes(state)
    ? states.filter((item) => item !== state)
    : canonicalStates([...states, state]);
}

/**
 * Text filter with a debounce and two-way URL sync.
 *
 * The draft updates immediately; the trimmed value reaches the URL after
 * `delay`. `committed` records the last value this field wrote so an echoing
 * URL change (our own commit) never overwrites newer keystrokes, while a real
 * external change (back/forward, "clear filters", a shared link) is adopted.
 */
function useDebouncedText(
  value: string,
  onCommit: (next: string) => void,
  delay: number = FILTER_DEBOUNCE_MS,
) {
  const [draft, setDraft] = useState(value);
  const committed = useRef(value);
  const current = useRef(value);
  const commit = useRef(onCommit);
  current.current = value;
  commit.current = onCommit;

  useEffect(() => {
    if (value !== committed.current) {
      committed.current = value;
      setDraft(value);
    }
  }, [value]);

  useEffect(() => {
    const next = draft.trim();
    if (next === current.current) {
      return;
    }
    const handle = window.setTimeout(() => {
      committed.current = next;
      commit.current(next);
    }, delay);
    return () => window.clearTimeout(handle);
  }, [draft, delay]);

  return [draft, setDraft] as const;
}

interface DebouncedTextInputProps {
  label: string;
  value: string;
  placeholder?: string;
  type?: "search" | "text";
  list?: string;
  onCommit: (next: string) => void;
}

function DebouncedTextInput({
  label,
  value,
  placeholder,
  type = "text",
  list,
  onCommit,
}: DebouncedTextInputProps) {
  const [draft, setDraft] = useDebouncedText(value, onCommit);
  return (
    <Input
      label={label}
      type={type}
      value={draft}
      placeholder={placeholder}
      list={list}
      onChange={(event) => setDraft(event.target.value)}
    />
  );
}

interface FieldsetProps {
  legend: string;
  children: ReactNode;
}

function FilterFieldset({ legend, children }: FieldsetProps) {
  return (
    <fieldset className="min-w-0 border-0 p-0">
      <legend className="text-caption font-medium text-text-secondary">{legend}</legend>
      <div className="mt-1.5 flex flex-wrap gap-2">{children}</div>
    </fieldset>
  );
}

interface IssueFiltersProps {
  filters: IssueListFilters;
  /** Repos visible in the current result, offered as a datalist. */
  repos: string[];
  /** Applies a filter/sort patch and resets the page to 1. */
  onApply: (patch: Partial<IssueListFilters>) => void;
  onClear: () => void;
}

/**
 * Filter bar (docs/09 §10.4): search, state multi-select, severity, repo,
 * language, attention/stuck shortcuts, sort/order, page size and clear.
 * Every control writes through the URL; nothing is stored locally except the
 * debounce drafts.
 */
export function IssueFilters({ filters, repos, onApply, onClear }: IssueFiltersProps) {
  const repoListId = useId();
  const canClear = hasActiveFilters(filters) || filters.page > 1;
  const pageSizeOptions = (
    PAGE_SIZE_OPTIONS.some((size) => size === filters.pageSize)
      ? [...PAGE_SIZE_OPTIONS]
      : [...PAGE_SIZE_OPTIONS, filters.pageSize]
  ).sort((left, right) => left - right);

  return (
    <section
      aria-labelledby="issue-filters-heading"
      className="rounded-card border border-border bg-surface p-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="issue-filters-heading" className="text-heading-sm font-semibold text-text">
          筛选与排序
        </h2>
        <Button variant="ghost" size="sm" onClick={onClear} disabled={!canClear}>
          <Eraser aria-hidden="true" className="size-3.5" />
          清除筛选
        </Button>
      </div>

      <div className="mt-3 grid gap-3 md:grid-cols-2 xl:grid-cols-4">
        <div className="md:col-span-2">
          <DebouncedTextInput
            label="搜索"
            type="search"
            value={filters.q}
            placeholder="标题或指纹，例如 panic"
            onCommit={(q) => onApply({ q })}
          />
        </div>
        <div>
          <DebouncedTextInput
            label="仓库"
            value={filters.repo}
            placeholder="example.com/app"
            list={repoListId}
            onCommit={(repo) => onApply({ repo })}
          />
          <datalist id={repoListId}>
            {repos.map((repo) => (
              <option key={repo} value={repo} />
            ))}
          </datalist>
        </div>
        <DebouncedTextInput
          label="语言"
          value={filters.language}
          placeholder="go / rust / python"
          onCommit={(language) => onApply({ language })}
        />
        <Select
          label="严重度"
          value={filters.severity}
          options={[
            { value: "", label: "全部严重度" },
            ...SEVERITY_FILTER_VALUES.map((value) => ({
              value,
              label: severityLabel(value),
            })),
          ]}
          onChange={(event) => {
            const next = event.target.value;
            onApply({ severity: isFilterSeverity(next) ? next : "" });
          }}
        />
        <Select
          label="排序"
          value={filters.sort}
          options={ISSUE_SORT_VALUES.map((value) => ({
            value,
            label: ISSUE_SORT_LABELS[value],
          }))}
          onChange={(event) => {
            const next = event.target.value;
            if (isIssueSort(next)) {
              onApply({ sort: next });
            }
          }}
        />
        <Select
          label="排序方向"
          value={filters.order}
          options={ISSUE_ORDER_VALUES.map((value) => ({
            value,
            label: ISSUE_ORDER_LABELS[value],
          }))}
          onChange={(event) => {
            const next = event.target.value;
            if (isIssueOrder(next)) {
              onApply({ order: next });
            }
          }}
        />
        <Select
          label="每页条数"
          value={String(filters.pageSize)}
          options={pageSizeOptions.map((value) => ({
            value: String(value),
            label: String(value),
          }))}
          onChange={(event) => {
            const parsed = Number(event.target.value);
            onApply({
              pageSize:
                Number.isSafeInteger(parsed) && parsed >= 1 && parsed <= MAX_PAGE_SIZE
                  ? parsed
                  : DEFAULT_PAGE_SIZE,
            });
          }}
        />
      </div>

      <div className="mt-4 flex flex-col gap-3 xl:flex-row xl:items-start xl:justify-between">
        <FilterFieldset legend="状态（可多选）">
          {ISSUE_STATE_VALUES.map((state) => {
            const pressed = filters.states.includes(state);
            return (
              <button
                key={state}
                type="button"
                aria-pressed={pressed}
                onClick={() => onApply({ states: toggleState(filters.states, state) })}
                className={chipClass(pressed)}
              >
                <span
                  aria-hidden="true"
                  className={clsx("state-badge__dot", `state-${state}`)}
                />
                {issueStateLabel(state)}
                {pressed ? <Check aria-hidden="true" className="size-3.5" /> : null}
              </button>
            );
          })}
        </FilterFieldset>

        <FilterFieldset legend="快捷筛选">
          <button
            type="button"
            aria-pressed={filters.attention}
            onClick={() => onApply({ attention: !filters.attention })}
            className={chipClass(filters.attention)}
          >
            <AlertTriangle aria-hidden="true" className="size-3.5" />
            仅需处理
            {filters.attention ? <Check aria-hidden="true" className="size-3.5" /> : null}
          </button>
          <button
            type="button"
            aria-pressed={filters.stuck}
            onClick={() => onApply({ stuck: !filters.stuck })}
            className={chipClass(filters.stuck)}
          >
            <Clock3 aria-hidden="true" className="size-3.5" />
            仅卡单
            {filters.stuck ? <Check aria-hidden="true" className="size-3.5" /> : null}
          </button>
        </FilterFieldset>
      </div>
    </section>
  );
}
