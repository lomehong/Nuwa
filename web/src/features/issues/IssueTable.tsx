import { clsx } from "clsx";
import { ArrowDown, ArrowRight, ArrowUp, ArrowUpDown, Copy } from "lucide-react";
import { Link } from "react-router-dom";

import {
  SeverityStone,
  StateBadge,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableRow,
} from "../../components/ui";
import {
  formatAge,
  formatCount,
  formatDateTime,
  isoDateTime,
  issueDetailPath,
  shortFingerprint,
} from "./format";
import { CiVerdictBadge, PrLink, StuckBadge } from "./IssueBadges";
import { IssueActionButton } from "./IssueActionButton";
import type { IssueListItem, IssueSort, IssueSortOrder } from "./types";

const ICON_ACTION =
  "inline-flex size-11 items-center justify-center rounded-card text-text-secondary " +
  "transition-colors duration-150 ease-out hover:bg-raised hover:text-text pointer-fine:size-9";

interface SortableHeaderProps {
  field: IssueSort;
  label: string;
  sort: IssueSort;
  order: IssueSortOrder;
  onSort: (field: IssueSort) => void;
  className?: string;
}

function SortableHeader({ field, label, sort, order, onSort, className }: SortableHeaderProps) {
  const active = sort === field;
  const direction = order === "asc" ? "ascending" : "descending";
  const Icon = !active ? ArrowUpDown : order === "asc" ? ArrowUp : ArrowDown;

  return (
    <TableHeaderCell
      aria-sort={active ? direction : "none"}
      className={className}
    >
      <button
        type="button"
        onClick={() => onSort(field)}
        aria-label={`按${label}排序${active ? `，当前${direction === "ascending" ? "升序" : "降序"}` : ""}`}
        className={clsx(
          "inline-flex min-h-11 items-center gap-1 rounded-sm font-medium transition-colors hover:text-text pointer-fine:min-h-9",
          active && "text-text",
        )}
      >
        {label}
        <Icon
          aria-hidden="true"
          className={clsx("size-3.5", active ? "text-accent" : "text-text-muted")}
        />
      </button>
    </TableHeaderCell>
  );
}

interface IssueTableProps {
  issues: IssueListItem[];
  sort: IssueSort;
  order: IssueSortOrder;
  onSort: (field: IssueSort) => void;
  onCopyFingerprint: (issue: IssueListItem) => void;
}

/**
 * Desktop issue table (docs/09 §10.4). The severity/title cell carries the
 * short fingerprint; the action column offers the state-aware owner action
 * (reopen/requeue) plus copy and detail. Sortable headers write the same
 * `sort`/`order` URL params as the filter bar.
 */
export function IssueTable({ issues, sort, order, onSort, onCopyFingerprint }: IssueTableProps) {
  return (
    <Table caption="问题单列表" label="问题单列表">
      <TableHead>
        <TableRow>
          <SortableHeader
            field="severity"
            label="严重度 / 标题"
            sort={sort}
            order={order}
            onSort={onSort}
            className="min-w-64"
          />
          <SortableHeader
            field="state"
            label="状态"
            sort={sort}
            order={order}
            onSort={onSort}
          />
          <TableHeaderCell>仓库</TableHeaderCell>
          <SortableHeader
            field="occurrences"
            label="次数"
            sort={sort}
            order={order}
            onSort={onSort}
            className="text-right"
          />
          <SortableHeader
            field="last_seen"
            label="最近出现"
            sort={sort}
            order={order}
            onSort={onSort}
          />
          <TableHeaderCell>PR / CI</TableHeaderCell>
          <TableHeaderCell>归属运行器</TableHeaderCell>
          <TableHeaderCell className="text-right">操作</TableHeaderCell>
        </TableRow>
      </TableHead>
      <TableBody>
        {issues.map((issue) => {
          const title = issue.title || "（无标题）";
          const detailPath = issueDetailPath(issue.fingerprint);
          return (
            <TableRow key={issue.fingerprint}>
              <TableCell className="min-w-64">
                <div className="flex items-start gap-2.5">
                  <SeverityStone severity={issue.severity} className="mt-0.5 shrink-0" />
                  <div className="min-w-0">
                    <Link
                      to={detailPath}
                      className="rounded-sm font-medium text-text transition-colors hover:text-accent"
                    >
                      {title}
                    </Link>
                    <button
                      type="button"
                      onClick={() => onCopyFingerprint(issue)}
                      aria-label={`复制指纹（完整）：${issue.fingerprint}`}
                      title="复制指纹（完整）"
                      className="mt-0.5 flex min-h-11 max-w-full items-center gap-1 rounded-sm text-left font-mono text-caption text-text-muted transition-colors hover:text-text pointer-fine:min-h-0"
                    >
                      <span className="truncate">{shortFingerprint(issue.fingerprint)}</span>
                      <Copy aria-hidden="true" className="size-3 shrink-0" />
                    </button>
                  </div>
                </div>
              </TableCell>
              <TableCell>
                <div className="flex flex-wrap items-center gap-1.5">
                  <StateBadge state={issue.state} />
                  {issue.stuck ? <StuckBadge /> : null}
                </div>
              </TableCell>
              <TableCell>
                <span
                  className="block max-w-48 truncate font-mono text-caption text-text-secondary"
                  title={issue.repo || undefined}
                >
                  {issue.repo || "—"}
                </span>
              </TableCell>
              <TableCell className="text-right">
                <span className="font-mono text-caption tabular-nums text-text">
                  {formatCount(issue.occurrences)}
                </span>
              </TableCell>
              <TableCell>
                <time
                  dateTime={isoDateTime(issue.last_seen_ms)}
                  title={formatDateTime(issue.last_seen_ms)}
                  className="whitespace-nowrap text-caption text-text-secondary"
                >
                  {formatAge(issue.last_seen_ms)}
                </time>
              </TableCell>
              <TableCell>
                <div className="flex flex-col items-start gap-1">
                  {issue.pr_url ? <PrLink url={issue.pr_url} title={title} /> : null}
                  <CiVerdictBadge verdict={issue.ci_verdict} />
                  {!issue.pr_url && !issue.ci_verdict ? (
                    <span className="text-caption text-text-muted">—</span>
                  ) : null}
                </div>
              </TableCell>
              <TableCell>
                <span
                  className="block max-w-40 truncate font-mono text-caption text-text-secondary"
                  title={issue.owner_runner_id ?? undefined}
                >
                  {issue.owner_runner_id ?? "未归属"}
                </span>
              </TableCell>
              <TableCell className="text-right">
                <div className="flex items-center justify-end gap-1">
                  <IssueActionButton
                    fingerprint={issue.fingerprint}
                    state={issue.state}
                    stuck={issue.stuck}
                  />
                  <button
                    type="button"
                    onClick={() => onCopyFingerprint(issue)}
                    aria-label={`复制指纹：${issue.fingerprint}`}
                    title="复制指纹"
                    className={ICON_ACTION}
                  >
                    <Copy aria-hidden="true" className="size-4" />
                  </button>
                  <Link
                    to={detailPath}
                    aria-label={`查看详情：${title}`}
                    title="查看详情"
                    className={ICON_ACTION}
                  >
                    <ArrowRight aria-hidden="true" className="size-4" />
                  </Link>
                </div>
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
