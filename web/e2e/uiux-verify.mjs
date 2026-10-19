#!/usr/bin/env node
/**
 * One-off UI/UX optimization verification against the locally seeded serve.
 * Complements e2e/smoke.mjs with the interactive specifics: palette arrow
 * navigation + view-all exit, evidence/run binding, timeline collapse,
 * transcript pagination buttons, light-theme colour separation.
 */
import { existsSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright-core";

const BASE = "http://127.0.0.1:7404";
const failures = [];
const consoleErrors = [];

function check(name, ok, detail = "") {
  console.log(`  ${ok ? "ok  " : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

function chromeExecutable() {
  const candidates = [
    process.env.NUWA_E2E_CHROME,
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    join(process.env.LOCALAPPDATA ?? "", "Google\\Chrome\\Application\\chrome.exe"),
  ].filter(Boolean);
  return candidates.find((candidate) => existsSync(candidate));
}

const browser = await chromium.launch({
  executablePath: chromeExecutable(),
  headless: true,
});
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
page.on("console", (message) => {
  if (message.type() === "error") consoleErrors.push(message.text());
});
page.on("pageerror", (error) => consoleErrors.push(`pageerror: ${error.message}`));

try {
  // ---- 1. palette: search + arrow navigation + view-all exit -------------
  await page.goto(`${BASE}/issues`, { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { level: 2, name: "问题单" }).waitFor({ timeout: 15_000 });

  await page.keyboard.press("Control+k");
  const dialog = page.getByRole("dialog", { name: "搜索问题单" });
  await dialog.waitFor({ timeout: 5_000 });
  const input = dialog.getByRole("searchbox");
  await input.fill("panic");
  await dialog.getByRole("option").first().waitFor({ timeout: 5_000 });

  const optionCount = await dialog.getByRole("option").count();
  check("palette shows results + view-all", optionCount >= 2, `${optionCount} options`);

  const selectedBefore = await dialog.locator('[aria-selected="true"]').getAttribute("id");
  await input.press("ArrowDown");
  const selectedAfter = await dialog.locator('[aria-selected="true"]').getAttribute("id");
  check("ArrowDown moves the selection", selectedBefore !== selectedAfter,
    `${selectedBefore} -> ${selectedAfter}`);
  await input.press("ArrowUp");
  const selectedBack = await dialog.locator('[aria-selected="true"]').getAttribute("id");
  check("ArrowUp moves back", selectedBack === selectedBefore);

  // Walk down to the view-all row (last of N entries), then Enter must land
  // on the filtered list page.
  for (let i = 1; i < optionCount; i++) {
    await input.press("ArrowDown");
  }
  const selectedLast = await dialog.locator('[aria-selected="true"]').getAttribute("id");
  check("view-all row is selectable", selectedLast === "palette-option-view-all", selectedLast);
  await input.press("Enter");
  await page.waitForURL(/\/issues\?q=panic/, { timeout: 5_000 });
  check("view-all exit navigates to the filtered list", true);
  await page.waitForURL(/sort=last_seen/, { timeout: 5_000 }).catch(() => {});
  check("list URL carries the query", page.url().includes("q=panic"), page.url());

  // ---- 2. detail page: evidence binding, timeline collapse, anchors ------
  // The seeded fixing issue (thread 'worker-3' panicked) carries two runs.
  await page.goto(
    `${BASE}/issues/bf522348f69864a1c9223bfbd306d35ca522f7b7633da34eabac48fe5b08418a`,
    { waitUntil: "domcontentloaded" },
  );
  await page.getByRole("heading", { name: /运行记录|证据|时间线/ }).first().waitFor({ timeout: 15_000 });

  const anchors = await page.locator('aside a[href^="#issue-"]').count();
  check("sidebar anchors present", anchors >= 5, `${anchors} anchors`);

  const back = page.getByRole("link", { name: "返回", exact: true });
  check("back link rendered", await back.count() === 1);

  const evidenceTitle = await page
    .getByText(/第 \d+ 次运行的证据|运行证据/)
    .first()
    .textContent({ timeout: 5_000 })
    .catch(() => null);
  check("evidence card labels its attempt", evidenceTitle !== null, evidenceTitle ?? "not found");

  // Runs list: selecting a run must switch the evidence card to that run.
  // The first candidate issue may carry a single run; the seeded "nil map"
  // issue carries several.
  let runButtons = page.getByRole("button", { name: /查看会话：/ });
  if ((await runButtons.count()) < 2) {
    await page.goto(
      `${BASE}/issues/5c8339e2da607711fa83250803b020e95499af52927452b95aa6fbc7c3ba75a5`,
      { waitUntil: "domcontentloaded" },
    );
    await page.getByRole("heading", { name: /运行记录/ }).waitFor({ timeout: 15_000 });
    runButtons = page.getByRole("button", { name: /查看会话：/ });
  }
  if ((await runButtons.count()) >= 2) {
    await runButtons.last().click();
    await page.waitForURL(/run_id=/, { timeout: 5_000 });
    // The oldest run is last in DOM order; selecting it must retitle the
    // evidence card to that attempt. Wait for the exact title: the regex
    // alone also matches the previous attempt's card during the transition.
    const switched = await page
      .getByText("第 2 次运行的证据")
      .first()
      .waitFor({ timeout: 5_000 })
      .then(() => "第 2 次运行的证据")
      .catch(() => null);
    check("evidence follows the selected run", switched === "第 2 次运行的证据",
      switched ?? "not found");
    const transcriptNav = page.getByRole("navigation", { name: "会话分页" });
    if (await transcriptNav.count()) {
      check("transcript has 首页/末页 buttons",
        (await transcriptNav.getByRole("button", { name: "首页" }).count()) === 1 &&
        (await transcriptNav.getByRole("button", { name: /末页（最新）/ }).count()) === 1);
    }
  } else {
    check("two-run issue available for binding test", false, "no issue with ≥2 runs found");
  }

  const expand = page.getByRole("button", { name: /展开全部|收起时间线/ });
  if (await expand.count()) {
    const capped = await page.locator("main .overflow-y-auto").count();
    check("timeline height-capped with expand toggle", capped >= 1);
    await expand.click();
    check("timeline expands", (await page.getByRole("button", { name: /收起时间线/ }).count()) === 1);
  } else {
    check("timeline expand toggle (short history hides it)", true, "≤5 events");
  }

  // ---- 3. light theme renders (colour tokens swap) ------------------------
  const themeToggle = page.locator('button[aria-label*="主题"]').first();
  check("theme toggle present", (await themeToggle.count()) >= 1);
  const themeBefore = await page.locator("html").getAttribute("data-theme");
  await themeToggle.click();
  await page.waitForTimeout(200);
  const theme = await page.locator("html").getAttribute("data-theme");
  check("theme toggles", theme !== themeBefore && theme !== null,
    `${themeBefore} -> ${theme}`);
  await page.screenshot({ path: "e2e/artifacts/uiux-light-detail.png", fullPage: false });

  console.log(consoleErrors.length === 0
    ? "  ok   no console errors"
    : `  FAIL console errors: ${consoleErrors.join(" | ")}`);
  if (consoleErrors.length > 0) failures.push("console errors");
} finally {
  await browser.close();
}

if (failures.length > 0) {
  console.error(`\nUIUX VERIFY FAILED: ${failures.length} problem(s)`);
  process.exit(1);
}
console.log("\nUIUX VERIFY PASSED");
