import { clsx } from "clsx";
import { useEffect, useRef, useState } from "react";

export interface FlashOnChangeProps {
  /** Current value; changing it re-triggers the one-shot highlight. */
  value: string | number;
  className?: string;
}

/**
 * docs/09 §11.5 — "轮询更新不闪烁：数字变化用轻微高亮". The span re-mounts with a
 * bumped key whenever `value` changes, replaying the `.nuwa-flash` animation;
 * the first render never flashes, so a page load is calm. The global
 * `prefers-reduced-motion` rule in tokens.css disables the animation.
 */
export function FlashOnChange({ value, className }: FlashOnChangeProps) {
  const [flashCount, setFlashCount] = useState(0);
  const previous = useRef(value);

  useEffect(() => {
    if (!Object.is(previous.current, value)) {
      previous.current = value;
      setFlashCount((count) => count + 1);
    }
  }, [value]);

  return (
    <span
      key={flashCount}
      className={clsx(flashCount > 0 && "nuwa-flash rounded-sm", className)}
    >
      {value}
    </span>
  );
}
