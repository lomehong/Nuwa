import { clsx } from "clsx";
import type {
  HTMLAttributes,
  ReactNode,
  TableHTMLAttributes,
  TdHTMLAttributes,
  ThHTMLAttributes,
} from "react";

export interface TableProps extends TableHTMLAttributes<HTMLTableElement> {
  /** Accessible description rendered as a `<caption>` (visually hidden by default). */
  caption?: ReactNode;
  captionVisible?: boolean;
  /** Label for the keyboard-scrollable region; falls back to the caption. */
  label?: string;
  containerClassName?: string;
}

/**
 * Semantic table inside a focusable, horizontally scrollable region so narrow
 * viewports keep the table usable from the keyboard (docs/09 §11.6).
 */
export function Table({
  caption,
  captionVisible = false,
  label,
  className,
  containerClassName,
  children,
  ...props
}: TableProps) {
  const regionLabel = label ?? (typeof caption === "string" ? caption : "数据表格");

  return (
    <div
      role="region"
      aria-label={regionLabel}
      tabIndex={0}
      className={clsx(
        "w-full overflow-x-auto rounded-card border border-border bg-surface",
        containerClassName,
      )}
    >
      <table className={clsx("w-full border-collapse text-left text-body", className)} {...props}>
        {caption ? (
          <caption
            className={clsx(
              captionVisible
                ? "px-4 pt-4 text-left text-body-sm font-medium text-text-secondary"
                : "sr-only",
            )}
          >
            {caption}
          </caption>
        ) : null}
        {children}
      </table>
    </div>
  );
}

export function TableHead({ className, children, ...props }: HTMLAttributes<HTMLTableSectionElement>) {
  return (
    <thead
      className={clsx(
        "border-b border-border bg-raised/60 text-caption font-medium text-text-muted",
        className,
      )}
      {...props}
    >
      {children}
    </thead>
  );
}

export function TableBody({ className, children, ...props }: HTMLAttributes<HTMLTableSectionElement>) {
  return (
    <tbody className={clsx("divide-y divide-border", className)} {...props}>
      {children}
    </tbody>
  );
}

export function TableRow({ className, children, ...props }: HTMLAttributes<HTMLTableRowElement>) {
  return (
    <tr className={clsx("transition-colors duration-150 hover:bg-raised/50", className)} {...props}>
      {children}
    </tr>
  );
}

export interface TableHeaderCellProps extends ThHTMLAttributes<HTMLTableCellElement> {
  scope?: "col" | "row";
}

export function TableHeaderCell({
  scope = "col",
  className,
  children,
  ...props
}: TableHeaderCellProps) {
  return (
    <th scope={scope} className={clsx("px-4 py-3 font-medium whitespace-nowrap", className)} {...props}>
      {children}
    </th>
  );
}

export function TableCell({ className, children, ...props }: TdHTMLAttributes<HTMLTableCellElement>) {
  return (
    <td className={clsx("px-4 py-3 align-middle", className)} {...props}>
      {children}
    </td>
  );
}
