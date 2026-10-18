import { clsx } from "clsx";
import { Moon, Sun } from "lucide-react";
import { useState } from "react";

import { applyTheme, nextTheme, resolveTheme, type Theme } from "../lib/theme";

export interface ThemeToggleProps {
  className?: string;
}

/**
 * Flips `<html data-theme>` and persists the choice under `nuwa-theme`, the
 * same key the pre-paint `public/theme-init.js` reads.
 */
export function ThemeToggle({ className }: ThemeToggleProps) {
  const [theme, setTheme] = useState<Theme>(() => resolveTheme());
  const target = nextTheme(theme);
  const label = target === "dark" ? "切换至深色主题" : "切换至浅色主题";
  const Icon = target === "dark" ? Moon : Sun;

  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={() => {
        const updated = nextTheme(theme);
        applyTheme(updated);
        setTheme(updated);
      }}
      className={clsx(
        "inline-flex size-11 shrink-0 items-center justify-center rounded-card border border-border",
        "bg-surface text-text-secondary transition-colors duration-150 ease-out",
        "hover:border-border-strong hover:text-text",
        className,
      )}
    >
      <Icon aria-hidden="true" className="size-4" />
    </button>
  );
}
