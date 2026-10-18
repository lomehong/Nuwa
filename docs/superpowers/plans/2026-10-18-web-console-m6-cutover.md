# Web Console M6 — Hardening and Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Finish the console polish (a11y, search, backoff, retention), remove the legacy server-rendered console, and deliver a locally verified 0.2.0 release candidate without pushing or deploying.

**Architecture:** The SPA becomes the only console at `/`. Legacy handlers in `ui.rs`, the Basic-auth middleware, origin guard, legacy settings handlers and the relaxed CSP are deleted; `/ui` and `/issue/{fp}` become permanent redirects and `/settings` falls through to the SPA. A command palette and page-level `/` focus implement the remaining §12 interactions. Shared polling helpers add exponential backoff. The final gate runs axe, a 5,000-issue benchmark, a production-DB-copy rehearsal and a rollback drill locally. Tagging, pushing and deploying remain separate, explicitly approved actions.

**Tech Stack:** React 19, TanStack Query v5, Playwright-core + axe-core, Axum, SQLite/rusqlite.

**Spec:** `docs/09-web-console-design.md` §10.1, §10.9, §11.6, §12, §14 M6, §15, §16; ADR-009

## Global Constraints

- No push, tag, deploy or production DB modification in this plan. Production ops are documented gates only.
- Credentials only from env vars; never write real tokens into the repo, docs, seeds or screenshots.
- No inline styles/scripts; axe reports zero serious/critical violations on the listed pages.
- `cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings` zero warnings; `cargo test -p nuwa -p shanhaijing` green; frontend typecheck/lint/test/build green.
- Existing guard/deliver behavior and vendored crates stay untouched.
- Version bump to `0.2.0` in `crates/nuwa/Cargo.toml`; release tag `v0.2.0` is a later manual gate.

---

## File structure

| File | Responsibility |
|---|---|
| `web/src/app/CommandPalette.tsx` | ⌘K/Ctrl+K issue search + navigation |
| `web/src/app/AppShell.tsx` | global shortcut, document title, nav polish |
| `web/src/lib/polling.ts` | exponential backoff interval helper |
| `web/src/features/**/queries.ts` | use the backoff helper |
| `web/src/components/ui/EmptyState.tsx` | live region only around text |
| `web/src/features/overview/PanelChrome.tsx` | alert only around error text |
| `web/src/styles/tokens.css` | light-theme stone contrast |
| `crates/nuwa/src/serve.rs` | remove legacy routes/middleware, redirects, strict CSP |
| `crates/nuwa/src/ui.rs` | delete |
| `crates/nuwa/src/settings.rs` | delete legacy HTTP handlers and `Shared` use |
| `crates/nuwa/src/main.rs` | remove `mod ui`, add serve-time prune |
| `crates/nuwa/Cargo.toml` | version 0.2.0 |
| `docs/` | 04 ADR status, 07 progress, 08 onboarding, 09 status, README |

---

### Task 1: Deferred a11y and visual polish

**Files:**
- Modify: `web/src/app/AppShell.tsx`
- Modify: `web/src/features/login/LoginPage.tsx`
- Modify: `web/src/components/ui/EmptyState.tsx`
- Modify: `web/src/features/overview/PanelChrome.tsx`
- Modify: `web/src/styles/tokens.css`
- Test: `web/src/app/AppShell.test.tsx`
- Test: `web/src/components/ui/EmptyState.test.tsx`
- Test: `web/src/features/overview/OverviewPage.test.tsx`
- Test: `web/src/styles/tokens.test.ts`

**Interfaces:**
- `document.title` becomes `总览 · 女娲`, `运行器 · 女娲`, `设置 · 女娲`; login is `登录 · 女娲`.
- `EmptyState` no longer wraps its action in `role="status"`.
- `PanelError` no longer wraps its retry button in `role="alert"`.
- Light stones reach ≥3:1 against `surface` and `bg`.

- [x] **Step 1: Write failing tests**

```tsx
it("updates the document title per route", async () => {
  signedIn();
  server.use(mockIssues());
  renderApp("/issues");
  await screen.findByRole("heading", { level: 2, name: "问题单" });
  expect(document.title).toBe("问题单 · 女娲");
});

it("keeps the empty-state action outside the status live region", () => {
  render(<EmptyState title="没有数据" action={<button type="button">重试</button>} />);
  const status = screen.getByRole("status");
  expect(status).not.toContainElement(screen.getByRole("button", { name: "重试" }));
});
```

In `tokens.test.ts` add:

```ts
const STONE_NAMES = ["stone-critical", "stone-high", "stone-medium", "stone-low", "stone-unknown"];
it("light stones reach 3:1 against both page and surface backgrounds", () => {
  for (const name of STONE_NAMES) {
    expect(contrastRatio(token("light", name), token("light", "surface")), name).toBeGreaterThanOrEqual(3);
    expect(contrastRatio(token("light", name), token("light", "bg")), name).toBeGreaterThanOrEqual(3);
  }
});
```

- [x] **Step 2: Run and watch fail**

`cd web && npx vitest run src/app/AppShell.test.tsx src/components/ui/EmptyState.test.tsx src/styles/tokens.test.ts` → FAIL.

- [x] **Step 3: Implement**

- `AppShell`: `useEffect(() => { document.title = title === "Nuwa 控制台" ? "女娲 · 补天" : `${title} · 女娲`; }, [title]);` `pageMeta` already returns titles.
- `LoginPage`: `useEffect(() => { document.title = "登录 · 女娲"; }, []);`
- `EmptyState`: remove `role="status"` from the `<section>`; wrap the icon/title/description block in `<div role="status">`; render `action` after that div.
- `PanelError`: remove `role="alert"` from `Card`; wrap the title/message in `<div role="alert">`; keep the retry button outside.
- Light stones in `tokens.css`:

```css
--stone-critical: #c62828;
--stone-high: #a94f00;
--stone-medium: #7a5b00;
--stone-low: #0b7a55;
--stone-unknown: #6a4bc4;
```

- Bottom navigation: after M4 all four slots are live; remove any residual disabled styling for 运行器/活动.

- [x] **Step 4: Run tests**

`cd web && npx vitest run src/app/AppShell.test.tsx src/components/ui src/styles src/features/overview` → PASS.

- [x] **Step 5: Commit**

```bash
git add web/src/app web/src/features/login/LoginPage.tsx web/src/components/ui/EmptyState.tsx web/src/features/overview/PanelChrome.tsx web/src/styles/tokens.css web/src/styles/tokens.test.ts
git commit -m "fix(web): deferred a11y and light-theme polish"
```

---

### Task 2: Command palette and page search shortcut

**Files:**
- Create: `web/src/app/CommandPalette.tsx`
- Create: `web/src/app/CommandPalette.test.tsx`
- Modify: `web/src/app/AppShell.tsx`
- Modify: `web/src/features/issues/IssuesPage.tsx`
- Modify: `web/src/features/issues/IssueFilters.tsx`

**Interfaces:**
- In AppShell, `Cmd/Ctrl+K` toggles the palette; Escape closes it through `useModalBehavior`.
- In IssuesPage, `/` focuses the search input when no editable element is focused; Escape blurs it.
- Palette queries `GET /issues?q=<q>&page_size=5&sort=last_seen&order=desc` (150 ms debounce) and links each result to `/issues/<fp>`.

- [x] **Step 1: Write failing tests**

```tsx
it("opens with Ctrl+K and navigates to a selected issue", async () => {
  server.use(mockMe(), ...mockOverviewResources(), mockIssues({ data: [issue] }));
  renderApp("/");
  await screen.findByRole("heading", { level: 1, name: "总览" });

  fireEvent.keyDown(document, { key: "k", ctrlKey: true });
  const dialog = await screen.findByRole("dialog", { name: "搜索问题单" });
  fireEvent.change(within(dialog).getByRole("searchbox"), { target: { value: "issue" } });
  fireEvent.click(await within(dialog).findByRole("link", { name: /issue/ }));

  expect(await screen.findByRole("heading", { level: 2, name: "问题单" })).toBeInTheDocument();
});
```

Issues page:

```tsx
it("focuses search on / and blurs on Escape", async () => {
  signedIn();
  server.use(mockIssues());
  renderApp("/issues");
  await screen.findByRole("heading", { level: 2, name: "问题单" });
  fireEvent.keyDown(document, { key: "/" });
  expect(screen.getByRole("searchbox", { name: "搜索" })).toHaveFocus();
  fireEvent.keyDown(document, { key: "Escape" });
  expect(screen.getByRole("searchbox", { name: "搜索" })).not.toHaveFocus();
});
```

- [x] **Step 2: Run and watch fail**

`cd web && npx vitest run src/app/CommandPalette.test.tsx src/features/issues/IssuesPage.test.tsx -t "focuses search"` → FAIL.

- [x] **Step 3: Implement**

`CommandPalette`:
- Portal + `role="dialog"`, `aria-label="搜索问题单"`, focus trap via `useModalBehavior`.
- Input `type="search"`, autofocus; 150 ms debounce; query enabled when trimmed length ≥ 1.
- Results: `Link` rows with title, short fingerprint, severity/state badges; loading skeleton; empty text “没有匹配的问题单”.
- Result click closes the palette (navigation also closes it).
- On open, clear the query.

AppShell:
```tsx
useEffect(() => {
  const onKeyDown = (event: KeyboardEvent) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      setPaletteOpen((open) => !open);
    }
  };
  document.addEventListener("keydown", onKeyDown);
  return () => document.removeEventListener("keydown", onKeyDown);
}, []);
```
Render `<CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} />` inside the authenticated shell.

IssuesPage/IssueFilters:
- Add a `searchInputRef` threaded to the `DebouncedTextInput` (add a `inputRef?: RefObject<HTMLInputElement | null>` prop).
- `useEffect` listens for `/` when `document.activeElement` is not `INPUT`, `TEXTAREA`, `SELECT`, or contenteditable; call `event.preventDefault(); searchInputRef.current?.focus();`.
- Escape blurs the search input only (does not clear the value).

- [x] **Step 4: Run tests**

`cd web && npx vitest run src/app/CommandPalette.test.tsx src/features/issues` → PASS.

- [x] **Step 5: Commit**

```bash
git add web/src/app/CommandPalette.tsx web/src/app/CommandPalette.test.tsx web/src/app/AppShell.tsx web/src/features/issues
git commit -m "feat(web): command palette and slash search shortcut"
```

---

### Task 3: Polling backoff

**Files:**
- Create: `web/src/lib/polling.ts`
- Create: `web/src/lib/polling.test.ts`
- Modify: `web/src/features/overview/queries.ts`
- Modify: `web/src/features/issues/queries.ts`
- Modify: `web/src/features/issue-detail/queries.ts`
- Modify: `web/src/features/runners/queries.ts`
- Modify: `web/src/features/events/queries.ts`

**Interfaces:**
- `backoffDelay(baseMs, failures, maxMs = 60_000): number` → base, 2×, 4×… capped.
- `backoffInterval(baseMs)` returns `(query: { state: { fetchFailureCount: number } }) => number | false`.

- [x] **Step 1: Write failing test**

```ts
import { backoffDelay } from "./polling";

it("backs off exponentially and caps at 60s", () => {
  expect(backoffDelay(10_000, 0)).toBe(10_000);
  expect(backoffDelay(10_000, 1)).toBe(20_000);
  expect(backoffDelay(10_000, 2)).toBe(40_000);
  expect(backoffDelay(10_000, 3)).toBe(60_000);
  expect(backoffDelay(10_000, 50)).toBe(60_000);
});
```

- [x] **Step 2: Run and watch fail**

`cd web && npx vitest run src/lib/polling.test.ts` → FAIL.

- [x] **Step 3: Implement and wire**

```ts
export function backoffDelay(baseMs: number, failures: number, maxMs = 60_000): number {
  if (failures <= 0) return baseMs;
  return Math.min(baseMs * 2 ** failures, maxMs);
}

export function backoffInterval(baseMs: number) {
  return (query: { state: { fetchFailureCount: number } }): number =>
    backoffDelay(baseMs, query.state.fetchFailureCount);
}
```

In each query hook replace `refetchInterval: CONST` with `refetchInterval: backoffInterval(CONST)`. Keep `refetchIntervalInBackground: false` and focus refetch. The existing cadence tests still assert one request per cycle after success (failure count 0).

- [x] **Step 4: Run all frontend query tests**

`cd web && npx vitest run src/lib src/features/overview src/features/issues src/features/issue-detail src/features/runners src/features/events` → PASS.

- [x] **Step 5: Commit**

```bash
git add web/src/lib/polling.ts web/src/lib/polling.test.ts web/src/features
git commit -m "feat(web): exponential backoff for polling"
```

---

### Task 4: Serve-time retention prune

**Files:**
- Modify: `crates/nuwa/src/serve.rs`
- Test: `crates/nuwa/src/serve.rs`

**Interfaces:**
- `event_retention_days()` reads `NUWA_EVENT_RETENTION_DAYS` (default 90, clamp 1–3650).
- On `serve` startup, call `store.prune(now, days, 365)` best-effort and log the deleted count.

- [x] **Step 1: Write failing test**

```rust
#[test]
fn serve_prunes_events_past_retention_but_keeps_audit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("prune.db");
    let store = Store::open(&path).expect("open");
    let now = crate::now_ms();
    store.record_event("fp", now - 120 * 86_400_000, "reported", "old", "r", "system").expect("event");
    store.record_event("fp", now, "config_changed", "keep", "r", "owner").expect("event");
    let deleted = prune_on_start(&store, now, 90).expect("prune");
    assert_eq!(deleted, 1);
    let kinds = store.list_events(&Default::default(), 100).expect("events");
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].kind, "config_changed");
}
```

(Use the actual `Store::record_event` signature and `EventQuery` default; if the test helper differs, reuse the M1a prune test pattern.)

- [x] **Step 2: Run and watch fail**

`cargo test -p nuwa serve::tests::serve_prunes_events_past_retention_but_keeps_audit` → FAIL.

- [x] **Step 3: Implement**

```rust
fn event_retention_days() -> u32 {
    std::env::var("NUWA_EVENT_RETENTION_DAYS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .map(|days| days.clamp(1, 3_650))
        .unwrap_or(90)
}

fn prune_on_start(store: &shanhaijing::Store, now: i64, days: u32) -> anyhow::Result<u64> {
    store.prune(now, days, 365).map_err(Into::into)
}
```

Call after `Store::open` in `run_serve`:

```rust
match prune_on_start(&store, now_ms(), event_retention_days()) {
    Ok(deleted) if deleted > 0 => println!("retention : pruned {deleted} rows"),
    Ok(_) => {}
    Err(error) => eprintln!("warning    : retention prune failed: {error}"),
}
```

- [x] **Step 4: Run serve tests and clippy**

```bash
cargo test -p nuwa serve::tests
cargo clippy --no-deps -p nuwa --all-targets -- -D warnings
```
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add crates/nuwa/src/serve.rs
git commit -m "feat(serve): prune expired telemetry on startup"
```

---

### Task 5: Remove the legacy console

**Files:**
- Delete: `crates/nuwa/src/ui.rs`
- Modify: `crates/nuwa/src/main.rs`
- Modify: `crates/nuwa/src/serve.rs`
- Modify: `crates/nuwa/src/settings.rs`
- Modify: `crates/nuwa/src/web.rs` (only if a guard is needed)
- Tests: `crates/nuwa/src/serve.rs`, `crates/nuwa/src/settings.rs`, `crates/nuwa/src/web.rs`

**Interfaces:**
- `GET /ui` and `GET /ui/{*rest}` → 308 to `/`.
- `GET /issue/{fp}` → 308 to `/issues/{fp}`.
- `GET /settings` → SPA (200, strict CSP).
- Legacy write routes (`/ui/reopen`, `/ui/sync`, `/settings/save`, `/settings/restart`) → 404.
- `/ingest`, `/health`, `/api/v1`, static assets unchanged.

- [x] **Step 1: Write failing routing tests**

```rust
#[tokio::test]
async fn legacy_console_redirects_to_the_spa() {
    let (_dir, app) = test_app();
    for (uri, location) in [("/ui", "/"), ("/ui/reopen", "/"), ("/issue/abc123", "/issues/abc123")] {
        let response = app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT, "{uri}");
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), location);
    }
}

#[tokio::test]
async fn legacy_write_routes_are_gone() {
    let (_dir, app) = test_app();
    for uri in ["/settings/save", "/settings/restart", "/ui/sync"] {
        let response = app.clone().oneshot(
            Request::builder().method("POST").uri(uri).body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn settings_spa_uses_the_strict_csp() {
    let (_dir, app) = test_app();
    let response = get(&app, "/settings").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(header_str(&response, header::CONTENT_SECURITY_POLICY).contains("style-src 'self'"));
    assert!(!header_str(&response, header::CONTENT_SECURITY_POLICY).contains("unsafe-inline"));
}
```

- [x] **Step 2: Run and watch fail**

`cargo test -p nuwa serve::tests::legacy_console_redirects_to_the_spa` → FAIL.

- [x] **Step 3: Implement**

- `main.rs`: remove `mod ui;`.
- `serve.rs`:
  - Remove `CSP_LEGACY`, the `legacy` path branch in `security_headers`, `legacy_state`, `ui_auth`, the `legacy_ui` router, and the `crate::ui::*` imports/routes.
  - Add redirect routes before the fallback:
    ```rust
    use axum::response::Redirect;
    use axum::routing::get;
    let redirects = Router::new()
        .route("/ui", get(|| async { Redirect::permanent("/") }))
        .route("/ui/{*rest}", get(|| async { Redirect::permanent("/") }))
        .route("/issue/{fp}", get(|axum::extract::Path(fp): axum::extract::Path<String>| async move {
            Redirect::permanent(&format!("/issues/{fp}"))
        }));
    ```
    Merge `redirects` into the final router before `.fallback(crate::web::serve)`.
  - Keep `check_auth`/ingest wiring; `Shared` remains for ingest only (or narrow it to a new `IngestState` type).
  - Delete the legacy tests in `serve.rs` (Basic auth, origin guard, relaxed CSP) and replace with the new routing tests.
- `settings.rs`: delete `get_settings`, `save_settings`, `restart_services`, their helpers/tests, the `Shared` import and `use crate::ui::esc`.
- `ui.rs`: delete the file.
- `web.rs`: keep the `/ingest`, `/health`, `/api` guards; `/ui` and `/issue` are handled by the router redirects.

- [x] **Step 4: Run Rust tests and clippy**

```bash
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p nuwa -p shanhaijing
```
Expected: PASS; no references to `crate::ui` remain (`grep -rn "crate::ui" crates/nuwa/src` returns nothing).

- [x] **Step 5: Commit**

```bash
git add -A crates/nuwa/src crates/nuwa/Cargo.toml
git commit -m "feat(serve)!: remove the legacy server-rendered console"
```

---

### Task 6: Version and documentation

**Files:**
- Modify: `crates/nuwa/Cargo.toml` (`version = "0.2.0"`)
- Modify: `Cargo.lock` (generated)
- Modify: `docs/04-decisions.md` (ADR-009 implementation status)
- Modify: `docs/07-open-items.md`
- Modify: `docs/08-onboarding.md`
- Modify: `docs/09-web-console-design.md` (status/implementation notes)
- Modify: `README.md`
- Modify: `AGENTS.md` (if commands/layout changed)

- [x] **Step 1: Bump version**

Set `crates/nuwa/Cargo.toml` to `version = "0.2.0"` and run `cargo check -p nuwa` to refresh `Cargo.lock`.

- [x] **Step 2: Update docs**

- docs/04 ADR-009: add an “Implementation status (2026-10-18)” section: M3–M6 completed; legacy console removed; version 0.2.0; release gate pending.
- docs/07: final M6 bullet with axe/benchmark/UAT evidence and the rollback procedure.
- docs/08: remove Basic-auth/old `/ui` instructions; document `http://<server>:7321/`, login token, settings page, service restart; keep `/ingest`, `/health`.
- docs/09: mark M4–M6 complete in §14; note `/ui` and `/issue/{fp}` redirects in §10.1/§16.
- README: quick start points at `/`, not `/ui`; mention the single binary embeds the SPA.
- AGENTS: layout/commands already include web; update any `/ui` references.

- [x] **Step 3: Verify no stale instructions**

```bash
grep -rn "/ui\|Basic 鉴权\|旧控制台" README.md docs/08-onboarding.md AGENTS.md docs/07-open-items.md
```
Only historical/ADR references may remain; operational docs must describe the SPA.

- [x] **Step 4: Commit**

```bash
git add crates/nuwa/Cargo.toml Cargo.lock docs README.md AGENTS.md
git commit -m "docs: cut over to the 0.2.0 web console"
```

---

### Task 7: axe, performance and production-copy rehearsal

**Files:**
- Modify: `web/package.json` (dev deps `axe-core`; optional `@axe-core/playwright`)
- Create: `web/e2e/a11y.mjs` (or extend `smoke.mjs`)
- Create: `web/e2e/bench.py` (optional)
- Modify: `.gitignore` for `web/e2e/artifacts/`

- [x] **Step 1: Install axe locally**

```bash
cd web && npm install --save-dev axe-core
```
No runtime dependency; the script injects `node_modules/axe-core/axe.min.js` into the page with `page.addScriptTag({ path })`.

- [x] **Step 2: Run axe on the core pages**

Extend `smoke.mjs` with:
```js
const axePath = require.resolve("axe-core/axe.min.js");
await page.addScriptTag({ path: axePath });
const results = await page.evaluate(async () => await axe.run(document, {
  runOnly: { type: "tag", values: ["wcag2a", "wcag2aa"] },
}));
const serious = results.violations.filter((v) => ["serious", "critical"].includes(v.impact));
if (serious.length) throw new Error(JSON.stringify(serious, null, 2));
```
Run for `/login`, `/`, `/issues`, `/issues/<fp>`, `/runners`, `/events`, `/settings`, both themes at 390 and 1440.

- [x] **Step 3: Benchmark with 5,000 issues**

Generate a temp DB (seed script extended to `--issues 5000`), start serve, and measure:
```bash
for i in $(seq 1 50); do curl -s -o /dev/null -w "%{time_total}\n" -b cookies.txt "http://127.0.0.1:7403/api/v1/issues?page=1&page_size=50"; done | sort -n | awk '{a[NR]=$1} END {print "p50="a[int(NR*0.5)]" p95="a[int(NR*0.95)]}'
```
Record p95 for `/issues` and `/overview`; acceptance: issues p95 < 300 ms, overview < 200 ms on this host. Browser-render the list at 1440 and 390 and confirm no layout overflow.

- [x] **Step 4: Production DB copy rehearsal (local, read-only source)**

- Copy a production DB **only if the operator has provided one**; otherwise use the seeded 5,000-issue DB and record that substitution.
- Run `nuwa serve` against the copy with a dummy UI token and a temp env file.
- Practice: login → four “open moments” (new/escalated/stuck/pr_open) → reopen a rejected issue → settings save of a non-secret key → restart dialog cancelled.
- Confirm no real credentials appear in screenshots/logs.

- [x] **Step 5: Rollback drill (local)**

- Keep the previous `v0.1.7` binary (or the pre-M6 commit's binary) available.
- Document: stop 0.2.0 → restore the previous binary → confirm schema v1 compatibility (no destructive migration) → `/ui` works again on the old binary.
- The rollback is a procedure only; do not deploy.

- [x] **Step 6: Commit artifacts**

```bash
git add web/package.json web/package-lock.json web/e2e .gitignore docs/07-open-items.md
git commit -m "test(web): axe, performance and cutover rehearsal"
```

---

### Task 8: Final review and release gate

- [x] **Step 1: Full verification**

```bash
cd web && npm run typecheck && npm run lint && npm test && npm run build
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p nuwa -p shanhaijing
cargo check --release -p nuwa
```

- [x] **Step 2: Whole-branch review**

Dispatch one final reviewer over the M4–M6 range with the spec, plans, reports and diff. Fix Critical/Important findings in one wave, re-review, and record residuals.

- [x] **Step 3: Release checklist (manual gate — do not execute)**

Document in docs/07:
- version `0.2.0`, tag `v0.2.0` (explicit approval required);
- `git push` (explicit approval required);
- release workflow builds web then cargo on both OS;
- deploy: backup DB + env, install binary, restart serve, smoke login/overview/settings, verify `/ui` redirect;
- rollback: previous binary + DB backup, confirm old `/ui`;
- acceptance evidence from axe/benchmark/UAT attached.

- [x] **Step 4: Commit**

```bash
git add docs/07-open-items.md
git commit -m "docs: 0.2.0 release and rollback checklist"
```

---

## Self-review

- Spec coverage: a11y §11.6 ✅; ⌘K/`/`/Esc §12 ✅; backoff §12 ✅; retention §13 ✅; legacy removal §14 M6/§16 ✅; docs/ADR/version §15.4 ✅; axe/performance/UAT §15.2–15.3 ✅; release/rollback §16 ✅.
- No push/tag/deploy; explicitly gated.
- Type consistency: `backoffDelay`, `event_retention_days`, redirect routes and version are referenced consistently.
