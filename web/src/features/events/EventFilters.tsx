import { clsx } from "clsx";
import { Check, Eraser, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";

import { Button, Input, Select } from "../../components/ui";
import { eventKindLabel } from "../overview/format";
import {
  AUDIT_KINDS,
  DEFAULT_EVENT_FILTERS,
  EVENT_KINDS,
  EVENT_RANGES,
  type EventFilters as EventFilterState,
  type EventRange,
} from "./filters";

const CHIP_BASE =
  "inline-flex min-h-11 shrink-0 items-center gap-1.5 rounded-full border px-3 text-caption " +
  "font-medium transition-colors duration-150 ease-out pointer-fine:min-h-9";
const CHIP_ACTIVE = "border-accent bg-accent/10 text-text";
const CHIP_IDLE =
  "border-border bg-surface text-text-secondary hover:border-border-strong hover:text-text";

function chipClass(active: boolean): string {
  return clsx(CHIP_BASE, active ? CHIP_ACTIVE : CHIP_IDLE);
}

/** True when every audit kind is selected and the window is the 30-day preset. */
export function isAuditView(filters: EventFilterState): boolean {
  if (filters.range !== "30d" || filters.kinds.length !== AUDIT_KINDS.length) {
    return false;
  }
  const selected = new Set(filters.kinds);
  return AUDIT_KINDS.every((kind) => selected.has(kind));
}

/** Whether anything besides the page differs from the default activity view. */
export function hasActiveEventFilters(filters: EventFilterState): boolean {
  return (
    filters.kinds.length > 0 ||
    filters.repo !== "" ||
    filters.fingerprint !== "" ||
    filters.range !== DEFAULT_EVENT_FILTERS.range ||
    filters.aggregate
  );
}

function toggleKind(kinds: string[], kind: string): string[] {
  if (kinds.includes(kind)) {
    return kinds.filter((item) => item !== kind);
  }
  // Keep canonical order so the URL and the request stay deterministic.
  return EVENT_KINDS.filter((item) => item === kind || kinds.includes(item));
}

interface CommitInputProps {
  label: string;
  value: string;
  placeholder: string;
  onCommit: (next: string) => void;
}

/**
 * Text filter that writes on Enter or blur instead of every keystroke: the
 * activity feed is an audit log, so partial terms must not spam the request
 * log. The draft is adopted again when the URL changes (clear/preset/back).
 */
function CommitInput({ label, value, placeholder, onCommit }: CommitInputProps) {
  const [draft, setDraft] = useState(value);

  useEffect(() => {
    setDraft(value);
  }, [value]);

  const commit = () => {
    const next = draft.trim();
    if (next !== value) {
      onCommit(next);
    }
  };

  return (
    <Input
      label={label}
      type="search"
      value={draft}
      placeholder={placeholder}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          commit();
        }
      }}
    />
  );
}

interface EventFiltersProps {
  filters: EventFilterState;
  /** Applies a filter patch and resets the page to 1 unless `page` is set. */
  onChange: (patch: Partial<EventFilterState>) => void;
}

/**
 * Activity filter bar (docs/09 §10.7): kind multi-select, the audit preset,
 * repo/fingerprint, range and hourly aggregation. Every control writes through
 * the URL; only the text drafts live in component state.
 */
export function EventFilters({ filters, onChange }: EventFiltersProps) {
  const auditView = isAuditView(filters);
  const canClear = hasActiveEventFilters(filters) || filters.page > 1;
  const rangeValue: EventRange = filters.range;

  return (
    <section
      aria-labelledby="event-filters-heading"
      className="rounded-card border border-border bg-surface p-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="event-filters-heading" className="text-heading-sm font-semibold text-text">
          筛选与视图
        </h2>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => onChange({ ...DEFAULT_EVENT_FILTERS })}
          disabled={!canClear}
        >
          <Eraser aria-hidden="true" className="size-3.5" />
          清除筛选
        </Button>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <span className="text-caption font-medium text-text-secondary">预设</span>
        <button
          type="button"
          aria-pressed={auditView}
          onClick={() => onChange({ kinds: [...AUDIT_KINDS], range: "30d", page: 1 })}
          className={chipClass(auditView)}
        >
          <ShieldCheck aria-hidden="true" className="size-3.5" />
          配置审计
          {auditView ? <Check aria-hidden="true" className="size-3.5" /> : null}
        </button>
      </div>

      <div className="mt-3 grid gap-3 md:grid-cols-2 xl:grid-cols-3">
        <CommitInput
          label="仓库"
          value={filters.repo}
          placeholder="example.com/app"
          onCommit={(repo) => onChange({ repo })}
        />
        <CommitInput
          label="指纹"
          value={filters.fingerprint}
          placeholder="问题单指纹前缀或完整值"
          onCommit={(fingerprint) => onChange({ fingerprint })}
        />
        <Select
          label="时间范围"
          value={rangeValue}
          options={EVENT_RANGES.map((range) => ({
            value: range.value,
            label: range.label,
          }))}
          onChange={(event) => {
            const next = event.target.value;
            if (EVENT_RANGES.some((range) => range.value === next)) {
              onChange({ range: next as EventRange });
            }
          }}
        />
      </div>

      <fieldset className="mt-4 min-w-0 border-0 p-0">
        <legend className="text-caption font-medium text-text-secondary">事件类型（可多选）</legend>
        <div className="mt-1.5 flex gap-2 overflow-x-auto pb-1 md:flex-wrap md:overflow-visible">
          {EVENT_KINDS.map((kind) => {
            const pressed = filters.kinds.includes(kind);
            return (
              <button
                key={kind}
                type="button"
                aria-pressed={pressed}
                onClick={() => onChange({ kinds: toggleKind(filters.kinds, kind) })}
                className={chipClass(pressed)}
              >
                {eventKindLabel(kind)}
                {pressed ? <Check aria-hidden="true" className="size-3.5" /> : null}
              </button>
            );
          })}
        </div>
      </fieldset>

      <label className="mt-4 inline-flex min-h-11 cursor-pointer items-center gap-2 text-body-sm text-text-secondary">
        <input
          type="checkbox"
          checked={filters.aggregate}
          onChange={(event) => onChange({ aggregate: event.target.checked })}
          className="size-4 rounded border-border accent-accent"
        />
        聚合历史上报（按小时）
      </label>
    </section>
  );
}
