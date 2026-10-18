import { clsx } from "clsx";
import { useId } from "react";

export interface SparklineDatum {
  /** Day label used in tooltips (for example `10-17`). */
  label: string;
  value: number;
}

export interface SparklineProps {
  data: SparklineDatum[];
  /** Metric name announced by assistive technology, e.g. "上报". */
  label: string;
  valueFormatter?: (value: number) => string;
  emptyLabel?: string;
  className?: string;
}

const VIEW_WIDTH = 100;
const VIEW_HEIGHT = 32;
const BAR_GAP = 1.5;
const MIN_BAR_HEIGHT = 2;

/**
 * Dependency-free bar sparkline used by the overview trends panel.
 *
 * Bars are SVG `<rect>`s with computed geometry attributes (never inline
 * styles) so the strict `style-src 'self'` CSP holds. The chart is a
 * keyboard-focusable `role="img"` with a total/peak summary, and every daily
 * value is also exposed as accessible text in a visually hidden table linked
 * through `aria-describedby` (pointer users still get per-bar `<title>`s).
 */
export function Sparkline({
  data,
  label,
  valueFormatter = String,
  emptyLabel = "暂无数据",
  className,
}: SparklineProps) {
  const descriptionId = useId();

  if (data.length === 0) {
    return <span className={clsx("text-caption text-text-muted", className)}>{emptyLabel}</span>;
  }

  const values = data.map((datum) => (Number.isFinite(datum.value) ? Math.max(0, datum.value) : 0));
  const max = Math.max(...values);
  // All-zero series keep a flat baseline instead of collapsing the viewBox.
  const scale = max > 0 ? max : 1;
  // A fixed gap would turn the bar width negative for long ranges (90 days);
  // shrink it so geometry stays valid for every bucket count.
  const gap = Math.min(BAR_GAP, VIEW_WIDTH / (data.length * 3));
  const barWidth = (VIEW_WIDTH - gap * (data.length - 1)) / data.length;

  const total = values.reduce((sum, value) => sum + value, 0);
  const peak = Math.max(...values);
  const peakIndex = values.indexOf(peak);
  const summary =
    `${label}，最近 ${data.length} 天共 ${valueFormatter(total)}，` +
    `峰值 ${valueFormatter(peak)}（${data[peakIndex]?.label ?? "—"}）`;

  return (
    <div className="min-w-0">
      <svg
        role="img"
        aria-label={summary}
        aria-describedby={descriptionId}
        tabIndex={0}
        viewBox={`0 0 ${VIEW_WIDTH} ${VIEW_HEIGHT}`}
        preserveAspectRatio="none"
        className={clsx("block h-9 w-full rounded-sm", className)}
      >
        <title>{summary}</title>
        {values.map((value, index) => {
          const height =
            value > 0
              ? Math.max(MIN_BAR_HEIGHT, (value / scale) * (VIEW_HEIGHT - 2))
              : MIN_BAR_HEIGHT;
          return (
            <rect
              key={`${data[index]?.label ?? index}-${index}`}
              x={index * (barWidth + gap)}
              y={VIEW_HEIGHT - height}
              width={barWidth}
              height={height}
              className={value > 0 ? "fill-accent" : "fill-border"}
            >
              <title>{`${data[index]?.label ?? ""}：${valueFormatter(value)}`}</title>
            </rect>
          );
        })}
      </svg>
      <table id={descriptionId} className="sr-only">
        <caption>{`${label} 每日数值（${data.length} 天）`}</caption>
        <thead>
          <tr>
            <th scope="col">日期</th>
            <th scope="col">数值</th>
          </tr>
        </thead>
        <tbody>
          {data.map((datum, index) => (
            <tr key={`${datum.label}-${index}`}>
              <th scope="row">{datum.label}</th>
              <td>{valueFormatter(values[index])}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
