#!/usr/bin/env node
/**
 * Local accessibility gate (docs/09 §15.2): runs axe-core WCAG 2.0/2.1 A+AA
 * against the core console pages at 390 and 1440 in both themes.
 *
 * Uses playwright-core + system Chrome (no browser download) and a seeded
 * `nuwa serve`. Serious/critical violations fail the process.
 *
 *   NUWA_E2E_BASE_URL=http://127.0.0.1:7404 NUWA_E2E_TOKEN=dummy npm run test:a11y
 */
import { existsSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";

import { chromium } from "playwright-core";

const require = createRequire(import.meta.url);
const AXE_PATH = require.resolve("axe-core/axe.min.js");

const BASE = (process.env.NUWA_E2E_BASE_URL ?? "").replace(/\/+$/, "");
const TOKEN = process.env.NUWA_E2E_TOKEN ?? "";
const ARTIFACTS = resolve(process.env.NUWA_E2E_ARTIFACTS ?? "e2e/artifacts");

function chromeExecutable() {
  const candidates = [
    process.env.NUWA_E2E_CHROME,
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    join(process.env.LOCALAPPDATA ?? "", "Google\\Chrome\\Application\\chrome.exe"),
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  ].filter(Boolean);
  const found = candidates.find((candidate) => existsSync(candidate));
  if (!found) {
    throw new Error("System Chrome not found; set NUWA_E2E_CHROME");
  }
  return found;
}

async function login(page) {
  await page.goto(`${BASE}/login`, { waitUntil: "domcontentloaded" });
  await page.getByRole("textbox", { name: "访问令牌" }).fill(TOKEN);
  await page.getByRole("button", { name: "进入控制台" }).click();
  await page.waitForURL((url) => url.pathname === "/", { timeout: 20_000 });
  await page.getByRole("heading", { level: 2, name: "总览" }).waitFor({ timeout: 20_000 });
}

async function runAxe(page, label) {
  const result = await page.evaluate(async () => {
    return await window.axe.run(document, {
      runOnly: { type: "tag", values: ["wcag2a", "wcag2aa"] },
    });
  });
  const serious = result.violations.filter((violation) =>
    ["serious", "critical"].includes(violation.impact),
  );
  if (serious.length > 0) {
    console.error(`  FAIL ${label}: ${serious.length} serious/critical`);
    for (const violation of serious) {
      console.error(`    ${violation.id} (${violation.impact}): ${violation.help}`);
      for (const node of violation.nodes.slice(0, 3)) {
        console.error(`      ${node.target.join(" ")}`);
      }
    }
  } else {
    console.log(`  ok   ${label}: ${result.violations.length} violations (none serious)`);
  }
  return serious.length;
}

const browser = await chromium.launch({
  executablePath: chromeExecutable(),
  headless: true,
});
let failures = 0;

try {
  mkdirSync(ARTIFACTS, { recursive: true });

  // Logged-out login page.
  for (const width of [390, 1440]) {
    const context = await browser.newContext({ viewport: { width, height: 900 } });
    await context.addInitScript({ path: AXE_PATH });
    const page = await context.newPage();
    await page.goto(`${BASE}/login`, { waitUntil: "networkidle" });
    failures += await runAxe(page, `login-${width}`);
    await context.close();
  }

  for (const theme of ["dark", "light"]) {
    for (const width of [390, 1440]) {
      const context = await browser.newContext({ viewport: { width, height: 900 } });
      await context.addInitScript({ path: AXE_PATH });
      await context.addInitScript((selected) => {
        window.localStorage.setItem("nuwa-theme", selected);
      }, theme);
      const page = await context.newPage();
      await login(page);

      const issue = await page.evaluate(async () => {
        const response = await fetch("/api/v1/issues?page_size=1", {
          credentials: "include",
        });
        const body = await response.json();
        return body.data?.[0]?.fingerprint ?? null;
      });

      const routes = [
        ["overview", "/"],
        ["issues", "/issues"],
        ...(issue ? [["detail", `/issues/${issue}`]] : []),
        ["runners", "/runners"],
        ["events", "/events"],
        ["settings", "/settings"],
      ];
      for (const [name, path] of routes) {
        await page.goto(`${BASE}${path}`, { waitUntil: "networkidle" });
        await page.waitForTimeout(300);
        failures += await runAxe(page, `${name}-${theme}-${width}`);
      }
      await page.screenshot({
        path: join(ARTIFACTS, `a11y-settings-${theme}-${width}.png`),
        fullPage: false,
      });
      await context.close();
    }
  }
} finally {
  await browser.close();
}

if (failures > 0) {
  console.error(`\naxe: ${failures} page/theme/width combinations failed`);
  process.exit(1);
}
console.log("\naxe: PASS (no serious/critical violations)");
