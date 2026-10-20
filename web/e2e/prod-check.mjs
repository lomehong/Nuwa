#!/usr/bin/env node
/**
 * Read-only walkthrough of the PRODUCTION console to confirm the deployed
 * build renders the implemented UI/UX optimizations with live data.
 * No write operation is ever clicked: only navigation, palette and theme.
 */
import { existsSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright-core";

const BASE = "http://159.75.227.240:7321";
const TOKEN = process.env.NUWA_UI_TOKEN ?? "";
const ARTIFACTS = "e2e/artifacts";
const failures = [];
const consoleErrors = [];

function check(name, ok, detail = "") {
  console.log(`  ${ok ? "ok  " : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

const exe = [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
  join(process.env.LOCALAPPDATA ?? "", "Google/Chrome/Application/chrome.exe"),
].find((candidate) => existsSync(candidate));

const browser = await chromium.launch({ executablePath: exe, headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
let loginComplete = false;
page.on("console", (message) => {
  // The pre-login /auth/me probe 401s by design (same as smoke.mjs).
  if (message.type() === "error" && loginComplete) consoleErrors.push(message.text());
});
page.on("pageerror", (error) => consoleErrors.push(`pageerror: ${error.message}`));

try {
  // ---- login --------------------------------------------------------------
  await page.goto(`${BASE}/login`, { waitUntil: "domcontentloaded" });
  await page.getByRole("textbox", { name: "访问令牌" }).fill(TOKEN);
  await page.getByRole("button", { name: "进入控制台" }).click();
  await page.getByRole("heading", { level: 2, name: "总览" }).waitFor({ timeout: 20_000 });
  loginComplete = true;
  console.log("  ok   logged in");

  // ---- overview: health pill, funnel bars, trends deltas, trust row -------
  const pill = page.locator('section[aria-label="全局健康条"] a', { hasText: "需处理" });
  check("health strip 需处理 pill", (await pill.count()) === 1,
    (await pill.textContent().catch(() => "") ?? "").trim());
  await page.screenshot({ path: `${ARTIFACTS}/prod-overview-top.png` });

  const funnelBars = page.locator('nav[aria-label="按状态筛选问题单"] svg rect');
  await funnelBars.first().waitFor({ timeout: 10_000 });
  check("funnel proportional bars (svg rects)", (await funnelBars.count()) >= 8,
    `${await funnelBars.count()} rects`);

  await page.locator("#overview-trends").scrollIntoViewIfNeeded();
  await page.waitForTimeout(400);
  const deltas = await page.getByText(/后半程新增|▲|▼|持平/).count();
  check("trends direction readouts", deltas >= 1, `${deltas} 处`);
  const trustRow = await page.getByText("误报 PR 率").count();
  check("trust metrics row (误报 PR 率)", trustRow === 1);
  await page.screenshot({ path: `${ARTIFACTS}/prod-overview-trends.png` });

  // ---- palette -------------------------------------------------------------
  await page.keyboard.press("Control+k");
  const dialog = page.getByRole("dialog", { name: "搜索问题单" });
  await dialog.waitFor({ timeout: 5_000 });
  const quick = await dialog.getByRole("option").count();
  check("palette quick commands (empty query)", quick === 4, `${quick} options`);
  const hint = await page.getByText("↑↓ 选择 · Enter 打开 · Esc 关闭").count();
  check("palette shortcut hint", hint === 1);
  await page.screenshot({ path: `${ARTIFACTS}/prod-palette-empty.png` });

  await dialog.getByRole("searchbox").fill("panic");
  await dialog.getByRole("option").first().waitFor({ timeout: 5_000 });
  const results = await dialog.getByRole("option").count();
  check("palette live search + view-all", results >= 2, `${results} options`);
  await page.keyboard.press("Escape");

  // ---- detail page of the escalated issue ----------------------------------
  await page.goto(`${BASE}/issues?state=escalated`, { waitUntil: "domcontentloaded" });
  await page.locator('a[href^="/issues/"]').first().click();
  await page.getByRole("heading", { name: /运行记录|时间线/ }).first().waitFor({ timeout: 15_000 });

  const anchors = await page.locator('aside a[href^="#issue-"]').count();
  check("sidebar 本页内容 anchors", anchors >= 5, `${anchors} anchors`);
  check("back link 返回", (await page.getByRole("link", { name: "返回", exact: true }).count()) === 1);
  const attempts = await page.getByText("修复尝试").count();
  check("修复尝试 metadata (new backend)", attempts === 1);
  const ignoreBtn = await page.getByRole("button", { name: /^忽略：/ }).count();
  check("忽略 button rendered (new backend)", ignoreBtn === 1);
  const evidenceTitle = await page
    .getByText(/第 \d+ 次运行的证据|运行证据|等待 runner/)
    .first()
    .textContent()
    .catch(() => null);
  check("evidence card present", evidenceTitle !== null, evidenceTitle ?? "");
  const stateIcons = await page.locator(".state-badge__icon").count();
  check("state badge icons (shape channel)", stateIcons >= 1, `${stateIcons} icons`);
  await page.screenshot({ path: `${ARTIFACTS}/prod-detail.png` });

  // ---- light theme ----------------------------------------------------------
  const themeBefore = await page.locator("html").getAttribute("data-theme");
  await page.locator('button[aria-label*="主题"]').first().click();
  await page.waitForTimeout(200);
  const themeAfter = await page.locator("html").getAttribute("data-theme");
  check("theme toggle works", themeAfter !== null && themeAfter !== themeBefore,
    `${themeBefore} -> ${themeAfter}`);
  await page.screenshot({ path: `${ARTIFACTS}/prod-detail-light.png` });

  console.log(consoleErrors.length === 0
    ? "  ok   no console errors"
    : `  FAIL console errors: ${consoleErrors.slice(0, 3).join(" | ")}`);
  if (consoleErrors.length > 0) failures.push("console errors");
} finally {
  await browser.close();
}

if (failures.length > 0) {
  console.error(`\nPROD CHECK FAILED: ${failures.length} problem(s)`);
  process.exit(1);
}
console.log("\nPROD CHECK PASSED");
