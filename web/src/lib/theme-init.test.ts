import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { THEME_STORAGE_KEY } from "./theme";

// Vitest runs from `web/`; use the project-relative path because import.meta.url
// is an http:// URL inside the jsdom environment.
const source = readFileSync("public/theme-init.js", "utf8");

describe("public/theme-init.js", () => {
  it("reads the same storage key as the theme module", () => {
    expect(THEME_STORAGE_KEY).toBe("nuwa-theme");
    expect(source).toContain(`"${THEME_STORAGE_KEY}"`);
  });

  it("only accepts dark and light theme values", () => {
    expect(source).toContain('theme !== "dark" && theme !== "light"');

    const accepted = [...source.matchAll(/theme !== "([a-z]+)"/g)].map((match) => match[1]);
    expect(accepted.sort()).toEqual(["dark", "light"]);
  });
});
