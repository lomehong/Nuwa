import { existsSync, readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

// Vitest runs from `web/`; use the project-relative path because import.meta.url
// is an http:// URL inside the jsdom environment.
const tokensCss = readFileSync("src/styles/tokens.css", "utf8");

type ThemeName = "dark" | "light";
type Rgb = [number, number, number];

const BADGE_VARIANTS = ["accent", "info", "success", "warning", "danger"] as const;
const STATE_VARIANTS = [
  "new",
  "triggered",
  "fixing",
  "pr_open",
  "merged",
  "rejected",
  "escalated",
] as const;

function declarationsFor(selector: RegExp): Record<string, string> {
  const match = tokensCss.match(selector);
  if (!match) {
    throw new Error(`tokens.css is missing ${selector}`);
  }
  const tokens: Record<string, string> = {};
  for (const declaration of match[1].matchAll(/--([a-z0-9_-]+):\s*(#[0-9a-fA-F]{6});/g)) {
    tokens[declaration[1]] = declaration[2];
  }
  return tokens;
}

const baseTokens = declarationsFor(/:root,\s*\[data-theme="dark"\]\s*\{([\s\S]*?)\n\}/);
const lightTokens = {
  ...baseTokens,
  ...declarationsFor(/\[data-theme="light"\]\s*\{([\s\S]*?)\n\}/),
};

const THEMES: Record<ThemeName, Record<string, string>> = {
  dark: baseTokens,
  light: lightTokens,
};

function token(theme: ThemeName, name: string): string {
  const value = THEMES[theme][name];
  if (!value) {
    throw new Error(`tokens.css is missing --${name} for the ${theme} theme`);
  }
  return value;
}

function hexToRgb(hex: string): Rgb {
  const value = hex.replace("#", "");
  return [0, 2, 4].map((offset) => Number.parseInt(value.slice(offset, offset + 2), 16)) as Rgb;
}

function toLinear(channel: number): number {
  const value = channel / 255;
  return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

function relativeLuminance(hex: string): number {
  const [red, green, blue] = hexToRgb(hex);
  return 0.2126 * toLinear(red) + 0.7152 * toLinear(green) + 0.0722 * toLinear(blue);
}

function contrastRatio(foreground: string, background: string): number {
  const first = relativeLuminance(foreground);
  const second = relativeLuminance(background);
  const lighter = Math.max(first, second);
  const darker = Math.min(first, second);
  return (lighter + 0.05) / (darker + 0.05);
}

/** Composite `foreground` at `alpha` over an opaque background in sRGB. */
function mixOver(foreground: string, background: string, alpha: number): string {
  const fg = hexToRgb(foreground);
  const bg = hexToRgb(background);
  const channels = fg.map((channel, index) =>
    Math.round(channel * alpha + bg[index] * (1 - alpha)),
  );
  return `#${channels.map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

interface ContrastPair {
  label: string;
  foreground: string;
  background: string;
}

function textPairs(theme: ThemeName): ContrastPair[] {
  const color = (name: string) => token(theme, name);
  return [
    { label: "body text on background", foreground: color("text"), background: color("bg") },
    {
      label: "body text on surface",
      foreground: color("text"),
      background: color("surface"),
    },
    {
      label: "secondary text on background",
      foreground: color("text-secondary"),
      background: color("bg"),
    },
    {
      label: "secondary text on surface",
      foreground: color("text-secondary"),
      background: color("surface"),
    },
    {
      label: "secondary text on raised",
      foreground: color("text-secondary"),
      background: color("raised"),
    },
    {
      label: "muted text on background",
      foreground: color("text-muted"),
      background: color("bg"),
    },
    {
      label: "muted text on surface",
      foreground: color("text-muted"),
      background: color("surface"),
    },
    {
      label: "muted text on raised",
      foreground: color("text-muted"),
      background: color("raised"),
    },
    {
      label: "table header text (raised/60 over surface)",
      foreground: color("text-secondary"),
      background: mixOver(color("raised"), color("surface"), 0.6),
    },
    {
      label: "neutral badge label",
      foreground: color("text-secondary"),
      background: color("raised"),
    },
    {
      label: "primary button label",
      foreground: color("on-accent"),
      background: color("accent"),
    },
    {
      label: "danger button label",
      foreground: color("on-danger"),
      background: color("danger"),
    },
    {
      label: "secondary button label",
      foreground: color("text"),
      background: color("raised"),
    },
    {
      label: "accent text on raised",
      foreground: color("accent"),
      background: color("raised"),
    },
    ...BADGE_VARIANTS.map((variant) => ({
      label: `${variant} badge label on 10% tint`,
      foreground: color("text"),
      background: mixOver(color(variant), color("surface"), 0.1),
    })),
    ...STATE_VARIANTS.map((state) => ({
      label: `${state} state label on 12% tint`,
      foreground: color("text"),
      background: mixOver(color(`state-${state}`), color("surface"), 0.12),
    })),
  ];
}

describe.each(["dark", "light"] as const)("%s theme", (theme) => {
  it("uses WCAG AA (4.5:1) text/background pairs", () => {
    const failures = textPairs(theme)
      .map((pair) => ({
        ...pair,
        ratio: contrastRatio(pair.foreground, pair.background),
      }))
      .filter((pair) => pair.ratio < 4.5)
      .map((pair) => `${pair.label}: ${pair.ratio.toFixed(2)}:1`);

    expect(failures, "text pairs below WCAG AA 4.5:1").toEqual([]);
  });
});

describe("self-hosted fonts", () => {
  it("declares local @font-face sources and ships the binaries plus OFL licenses", () => {
    const assets = [
      "InterVariable.woff2",
      "JetBrainsMono-Regular.woff2",
      "JetBrainsMono-Bold.woff2",
      "LICENSE-Inter.txt",
      "LICENSE-JetBrainsMono.txt",
    ];

    for (const asset of assets) {
      expect(existsSync(`src/assets/fonts/${asset}`)).toBe(true);
    }

    expect(tokensCss).toContain(
      'url("../assets/fonts/InterVariable.woff2") format("woff2")',
    );
    expect(tokensCss).toContain(
      'url("../assets/fonts/JetBrainsMono-Regular.woff2") format("woff2")',
    );
    expect(tokensCss).toContain(
      'url("../assets/fonts/JetBrainsMono-Bold.woff2") format("woff2")',
    );
  });

  it("puts the self-hosted families first in the UI and data stacks", () => {
    expect(tokensCss).toMatch(/--font-family-sans:\s*"Inter",/);
    expect(tokensCss).toMatch(/--font-family-mono:\s*"JetBrains Mono",/);
  });
});
