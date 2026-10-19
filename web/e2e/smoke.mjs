#!/usr/bin/env node
/**
 * M5 console smoke test (docs/09 §15.2).
 *
 * Uses playwright-core with the system Chrome — no browser download. The
 * script drives a locally seeded `nuwa serve` instance:
 *
 *   NUWA_E2E_BASE_URL=http://127.0.0.1:7404 \
 *   NUWA_E2E_TOKEN=dummy \
 *   npm run test:e2e
 *
 * Optional:
 *   NUWA_E2E_CHROME     absolute Chrome/Chromium executable
 *   NUWA_E2E_ARTIFACTS  screenshot directory (default web/e2e/artifacts)
 *
 * Every failure (flow assertion, console error, page error, horizontal
 * overflow, inline style) is collected and the process exits non-zero.
 */

import { existsSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright-core";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const BASE = (process.env.NUWA_E2E_BASE_URL ?? "").replace(/\/+$/, "");
const TOKEN = process.env.NUWA_E2E_TOKEN ?? "";
const ARTIFACTS = resolve(
  process.env.NUWA_E2E_ARTIFACTS ?? join(SCRIPT_DIR, "artifacts"),
);

const failures = [];
const consoleEntries = [];
const consoleErrors = [];
const pageErrors = [];
const writeRequests = [];

/** Responses before login completes; only `/auth/me` 401s are expected. */
let pendingLoginProbe401 = 0;
let loginComplete = false;

function fail(message) {
  failures.push(message);
  console.error(`  FAIL ${message}`);
}

function pass(message) {
  console.log(`  ok   ${message}`);
}

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
    throw new Error(
      "System Chrome not found. Set NUWA_E2E_CHROME to a Chrome/Chromium executable " +
        "(playwright-core does not download browsers).",
    );
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

/**
 * `/settings` is a plain SPA route since the legacy console was removed, so a
 * direct navigation (the operator's bookmark/refresh path) must render the
 * settings page like any other route.
 */
async function openSettings(page) {
  await page.goto(`${BASE}/settings`, { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { level: 2, name: "设置" }).waitFor({ timeout: 15_000 });
}

/** Layout invariants for one page at the current viewport. */
async function auditLayout(page, label) {
  await page.waitForTimeout(350);
  const result = await page.evaluate(() => ({
    path: `${location.pathname}${location.search}`,
    overflow: document.documentElement.scrollWidth > window.innerWidth + 1,
    inlineStyles: document.querySelectorAll("[style]").length,
    firstInlineStyle:
      document.querySelector("[style]")?.outerHTML.slice(0, 160) ?? null,
  }));
  const screenshot = join(ARTIFACTS, `${label}.png`);
  await page.screenshot({ path: screenshot });
  if (result.overflow) {
    fail(`${label}: horizontal overflow at ${result.path}`);
  }
  if (result.inlineStyles > 0) {
    fail(`${label}: ${result.inlineStyles} inline style attribute(s): ${result.firstInlineStyle}`);
  }
  if (!result.overflow && result.inlineStyles === 0) {
    pass(`${label}: no horizontal overflow, no inline styles`);
  }
  return result;
}

async function runFlow(page) {
  // 1. Overview
  await page.goto(`${BASE}/`, { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { level: 2, name: "总览" }).waitFor();
  pass("overview rendered");

  // 2. Issue list filtered to rejected, reopen without confirmation.
  await page.goto(`${BASE}/issues?state=rejected`, { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { level: 2, name: "问题单" }).waitFor();
  const reopen = page.getByRole("button", { name: /重新打开/ }).first();
  await reopen.waitFor({ timeout: 15_000 });
  const rejectedRows = await page
    .locator("tbody tr, [data-state='rejected']")
    .count();
  if (rejectedRows === 0) {
    fail("rejected filter rendered no rejected issue");
  }
  await reopen.click();
  await page.getByText("已重新入队", { exact: true }).waitFor({ timeout: 10_000 });
  pass("rejected issue reopened (toast 已重新入队)");

  // 3. Runners: restart dialog explains the impact, then cancel.
  await page.goto(`${BASE}/runners`, { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { level: 2, name: "运行器" }).waitFor();
  const restart = page.getByRole("button", { name: /^重启 unit：/ }).first();
  await restart.waitFor({ timeout: 15_000 });
  await restart.click();
  const dialog = page.getByRole("dialog", { name: "重启 runner unit？" });
  await dialog.waitFor({ timeout: 10_000 });
  const impact = await dialog.innerText();
  if (!impact.includes("同一 unit 承载的多个 runner 会一起重启")) {
    fail("restart dialog is missing the shared-unit impact warning");
  }
  if (!impact.includes("当前正在修复")) {
    fail("restart dialog is missing the active-fingerprint warning");
  }
  await dialog.getByRole("button", { name: "取消" }).click();
  await dialog.waitFor({ state: "detached", timeout: 10_000 });
  if (writeRequests.some((path) => /\/runners\/[^/]+\/restart$/.test(path))) {
    fail("cancelling the restart dialog still POSTed /runners/{id}/restart");
  } else {
    pass("restart dialog impact shown; cancel sent no request");
  }

  // 4. Events: audit preset and badge.
  await page.goto(`${BASE}/events`, { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { level: 2, name: "活动" }).waitFor();
  await page.getByRole("button", { name: "配置审计" }).click();
  await page.getByText("审计视图", { exact: true }).waitFor({ timeout: 10_000 });
  pass("audit preset applied (badge 审计视图)");

  // 5. Settings: change Poll interval and save. Never touch restart.
  await openSettings(page);
  const interval = page.getByLabel("Poll interval");
  await interval.waitFor({ timeout: 15_000 });
  await interval.fill("60");
  const saveBar = page.getByTestId("settings-save-bar");
  await saveBar.waitFor({ timeout: 10_000 });
  // The page also renders a header save action; click the sticky bar's one.
  await saveBar.getByRole("button", { name: "保存更改" }).click();
  await page.getByText("设置已保存", { exact: true }).waitFor({ timeout: 15_000 });
  await saveBar.waitFor({ state: "detached", timeout: 15_000 });
  if ((await page.getByText(/未保存/).count()) > 0) {
    fail("dirty badge still visible after saving settings");
  } else {
    pass("settings saved (toast 设置已保存, dirty bar gone)");
  }
  if (writeRequests.some((path) => path.endsWith("/settings/restart"))) {
    fail("settings flow POSTed /settings/restart");
  } else {
    pass("restart button was not used");
  }

  // 6. Responsive screenshots and layout invariants.
  for (const [width, height] of [
    [390, 844],
    [1024, 900],
    [1440, 900],
  ]) {
    await page.setViewportSize({ width, height });
    await page.goto(`${BASE}/`, { waitUntil: "domcontentloaded" });
    await page.getByRole("heading", { level: 2, name: "总览" }).waitFor();
    await auditLayout(page, `overview-${width}`);
    await openSettings(page);
    await auditLayout(page, `settings-${width}`);
  }
}

async function main() {
  if (!BASE || !TOKEN) {
    console.error("NUWA_E2E_BASE_URL and NUWA_E2E_TOKEN are required.");
    process.exit(2);
  }
  mkdirSync(ARTIFACTS, { recursive: true });
  console.log(`Nuwa E2E: ${BASE} (artifacts: ${ARTIFACTS})`);

  const browser = await chromium.launch({
    executablePath: chromeExecutable(),
    headless: true,
  });

  let loggedIn = false;
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();

    page.on("console", (message) => {
      if (message.type() !== "error") {
        return;
      }
      // The login page probes /auth/me and the browser logs that 401. Keep
      // every entry with its timing so only those pre-login probes are
      // dropped once the matching response has been counted.
      consoleEntries.push({
        text: message.text(),
        beforeLogin: !loginComplete,
      });
    });
    page.on("pageerror", (error) => pageErrors.push(String(error)));
    page.on("response", (response) => {
      if (loginComplete || response.status() !== 401) {
        return;
      }
      const url = new URL(response.url());
      if (url.pathname === "/api/v1/auth/me") {
        pendingLoginProbe401 += 1;
      }
    });
    page.on("request", (request) => {
      if (request.method() !== "POST") {
        return;
      }
      const url = new URL(request.url());
      if (!url.pathname.startsWith("/api/v1/")) {
        return;
      }
      writeRequests.push(url.pathname);
    });

    await login(page);
    loggedIn = true;
    loginComplete = true;
    pass("logged in through NUWA_UI_TOKEN");

    try {
      await runFlow(page);
    } catch (error) {
      fail(`flow aborted: ${error instanceof Error ? error.message : String(error)}`);
      await page
        .screenshot({ path: join(ARTIFACTS, "failure.png") })
        .catch(() => undefined);
    }

    await context.close();
  } catch (error) {
    fail(`browser/login failure: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    await browser.close();
  }

  // Drop only the pre-login console errors that correspond to an observed
  // `/auth/me` 401; every other error is still reported.
  let expectedLogin401 = pendingLoginProbe401;
  for (const entry of consoleEntries) {
    if (entry.beforeLogin && expectedLogin401 > 0 && entry.text.includes("401")) {
      expectedLogin401 -= 1;
      continue;
    }
    consoleErrors.push(entry.text);
  }

  for (const error of consoleErrors) {
    fail(`console error: ${error}`);
  }
  for (const error of pageErrors) {
    fail(`page error: ${error}`);
  }

  if (failures.length > 0) {
    console.error(`\nE2E FAILED: ${failures.length} problem(s)`);
    process.exitCode = 1;
    return;
  }
  if (!loggedIn) {
    console.error("\nE2E FAILED: not logged in");
    process.exitCode = 1;
    return;
  }
  console.log("\nE2E PASSED");
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
