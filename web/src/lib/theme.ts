/**
 * Theme plumbing shared by `public/theme-init.js` and `<ThemeToggle>`.
 *
 * The pre-paint script and this module must agree on the storage key and the
 * accepted values; keep them in sync (the script cannot import TypeScript).
 * All reads are defensive because localStorage and matchMedia can throw or be
 * unavailable (privacy modes, embedded webviews, tests).
 */

export const THEME_STORAGE_KEY = "nuwa-theme";

export type Theme = "dark" | "light";

export function isTheme(value: unknown): value is Theme {
  return value === "dark" || value === "light";
}

export function getSystemTheme(): Theme {
  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }
  return "dark";
}

export function getStoredTheme(): Theme | null {
  try {
    const value = window.localStorage.getItem(THEME_STORAGE_KEY);
    return isTheme(value) ? value : null;
  } catch {
    return null;
  }
}

/**
 * Resolve the theme that is currently in effect. `data-theme` wins because
 * `theme-init.js` already merged localStorage and the system preference before
 * first paint; on a bare test DOM we fall back to the same precedence.
 */
export function resolveTheme(): Theme {
  const current = document.documentElement.getAttribute("data-theme");
  if (isTheme(current)) {
    return current;
  }
  return getStoredTheme() ?? getSystemTheme();
}

export function applyTheme(theme: Theme, persist = true): void {
  document.documentElement.setAttribute("data-theme", theme);
  if (!persist) {
    return;
  }
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // Storage unavailable: the attribute still switches the live document.
  }
}

export function nextTheme(theme: Theme): Theme {
  return theme === "dark" ? "light" : "dark";
}
