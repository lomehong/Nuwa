# Web 控制台 M3 SPA 核心 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 React 19 + TypeScript + Vite 构建可工作的 SPA 核心：登录、应用外壳、总览、问题单列表、问题单详情（含转录），并建立设计令牌/双主题。替换 `web.rs` 的占位页；运行器/活动/设置/动作/切流留给 M4–M6。

**Architecture:** Vite 构建产物由 `rust-embed` 内嵌；开发用 Vite 代理 `/api` 到 `nuwa serve`。TanStack Query 管服务端状态，React Router 管路由；登录后使用 HttpOnly 会话 Cookie，前端读取 `nuwa_csrf` 并在写请求带 `X-NUWA-CSRF`。设计令牌用 CSS 变量 + Tailwind v4，暗色默认/浅色可切。

**Tech Stack:** React 19、TypeScript、Vite、React Router、TanStack Query、Tailwind CSS v4、Zod、lucide-react、Vitest/RTL/MSW。

**Spec:** docs/09 §10（页面）、§11（视觉/无障碍）、§12（交互）、§5.4（静态）、§6（鉴权）；ADR-009。

## Global Constraints

- 不修改后端行为；只消费 `/api/v1`。
- 单二进制：release 先 `npm ci && npm run build` 再 cargo；`web/dist` 不提交（保留 `.gitkeep`）。
- 暗色默认 + 浅色切换；令牌见 docs/09 §11.1；五色石严重度与状态色分离。
- Strict CSP：不使用内联脚本/样式；主题初始化用 `theme-init.js`（public）。
- 所有 API 请求同源 + credentials；401 跳登录；写请求带 CSRF；错误用统一 envelope。
- 关键页面可键盘操作、语义化表格、表单 label；移动端 <768 卡片化。
- 每个任务一个 commit；不 push/tag/deploy。

---

### Task 1: Vite/React 脚手架与构建链

**Files:** `web/package.json`, `web/package-lock.json`, `web/vite.config.ts`, `web/tsconfig*.json`, `web/index.html`, `web/src/main.tsx`, `web/src/app/App.tsx`, `web/src/index.css`, `web/public/theme-init.js`, `.gitignore`, `.github/workflows/{ci,release}.yml`
**Interfaces:** `npm run dev|build|typecheck|lint|test`; Vite proxy `/api` and `/ingest` → `http://127.0.0.1:7321`; `web/dist` output.
**Steps:**
1. Create the Vite React TS app; dependencies: react/react-dom/react-router-dom/@tanstack/react-query/zod/clsx/lucide-react; dev: vite/@vitejs/plugin-react/typescript/tailwindcss/@tailwindcss/vite/vitest/@testing-library/react/@testing-library/jest-dom/jsdom/msw/eslint/typescript-eslint.
2. `vite.config.ts` with `@vitejs/plugin-react`, `@tailwindcss/vite`, proxy, and `test` (jsdom).
3. `index.html` loads `/theme-init.js` (external, before app) and `/src/main.tsx`.
4. Minimal `App.tsx` renders a placeholder; `npm run build` produces index/assets.
5. Add `web` job to CI (Node 24, npm ci, typecheck, lint, test, build). Release: setup Node + `npm ci && npm run build` in `web/` before cargo on both OS.
6. `.gitignore` keeps `/web/dist/*` except `.gitkeep`; ensure `web/dist/.gitkeep`.
7. Verify `npm ci`, typecheck, test, build; cargo tests still green (static tests are build-independent).
**Commit:** `feat(web): vite react scaffold and frontend build pipeline`.

---

### Task 2: 设计令牌、主题与基础组件

**Files:** `web/src/styles/tokens.css`, `web/src/index.css`, `web/src/components/ui/*`, `web/src/lib/theme.ts`, `web/src/components/ThemeToggle.tsx`, tests
**Interfaces:** CSS variables from docs/09 §11.1 (`--bg`, `--surface`, `--raised`, `--border`, `--text`, `--text-secondary`, `--text-muted`, `--accent`, `--success`, `--warning`, `--danger`, `--info`, `--stone-*`); `[data-theme=light]` overrides; `SeverityStone`, `StateBadge`, `Card`, `Button`, `Input`, `Select`, `Table`, `Badge`, `EmptyState`, `Skeleton`, `Toast` (context), `ConfirmDialog`.
**Steps:**
1. Add tokens + base styles (font stacks, focus-visible ring, reduced motion).
2. Build primitives with accessible semantics and class-only colors (no inline style).
3. Theme: read localStorage + prefers-color-scheme via `theme-init.js`; `ThemeToggle` flips `data-theme`.
4. Tests: severity/state classes, theme toggle persists, button/input labels, empty state.
**Commit:** `feat(web): design tokens, themes and UI primitives`.

---

### Task 3: API 客户端、登录、路由与 AppShell

**Files:** `web/src/lib/api.ts`, `web/src/lib/auth.tsx`, `web/src/app/router.tsx`, `web/src/app/AppShell.tsx`, `web/src/features/login/LoginPage.tsx`, `web/src/features/overview/…` (route placeholder), tests
**Interfaces:**
- `apiFetch<T>(path, init?)` same-origin, `credentials: "include"`, JSON error envelope, 401 → `/login`, reads `nuwa_csrf` cookie and sets `X-NUWA-CSRF` for non-GET.
- `AuthProvider` bootstraps `/auth/me`; `useAuth`; `LoginPage` posts `{token}` to `/auth/login`; logout posts `/auth/logout`.
- Router routes `/login`, `/` (overview), `/issues`, `/issues/:fp`; unknown → redirect `/`.
- `AppShell`: sidebar nav (总览/问题单/运行器/活动/设置 — latter disabled/`Soon` until M4/M5), topbar (search placeholder, theme toggle, logout), HealthStrip (serve/runners/queue/active/quota from `/overview`), mobile drawer/bottom nav.
**Steps:**
1. API client + CSRF + error type + tests (fetch mock/MSW).
2. Auth provider/login/logout/401 handling.
3. Router + shell + health strip; login redirects to intended route.
4. Tests: login success/failure, 401 redirect, CSRF header, shell nav.
**Commit:** `feat(web): api client, auth flow and app shell`.

---

### Task 4: 总览页

**Files:** `web/src/features/overview/*`, `web/src/components/charts/Sparkline.tsx`, tests
**Interfaces:** Consume `/api/v1/overview?repo=` and `/api/v1/metrics/trends?range=14d`; render:
- attention queue (escalated/stuck/rejected/ci_timeout) with actions (link to detail; reopen/requeue in M4).
- active runs phase stepper (claim→repair→verify→deliver→ci), elapsed, turns/tokens; idle queue preview.
- health runner tiles (online/offline/standing_down/error), CI 24h, quota.
- pipeline funnel counts; click filters issues.
- trends (14d) custom SVG bars/sparklines; recent events from `/events?page_size=8`.
- loading/empty/error/offline states; pause polling when hidden.
**Tests:** MSW overview/trends/events; render counts/attention/active/health; empty state; poll pause not unit-required.
**Commit:** `feat(web): ops overview with attention, runs, health and trends`.

---

### Task 5: 问题单列表

**Files:** `web/src/features/issues/*`, tests
**Interfaces:** `/issues` with URL-synced filters (`state,severity,repo,language,q,stuck,attention,sort,order,page,page_size`); table desktop/cards mobile; severity stone/state badge; PR link; pagination; row actions (copy fingerprint, open detail); empty/error/loading.
**Steps:** query keys + debounce search; filter chips/selects; sortable headers; pagination; URL sync; tests for filter/sort/page/empty and URL round-trip.
**Commit:** `feat(web): issue list with filters, sort and pagination`.

---

### Task 6: 问题单详情与转录

**Files:** `web/src/features/issue-detail/*`, `web/src/features/transcript/*`, tests
**Interfaces:** `/issues/:fp` + `/issues/:fp/transcript?run_id=&page=`:
- header severity/title/state/repo/language/fingerprint copy/occurrences/times/PR.
- timeline (kind/detail/actor/time, relative + absolute tooltip).
- evidence: latest run root cause/change stat/verification detail/CI; stack sample copy.
- runs list (last 10) with phase/outcome/tokens/branch/commit.
- transcript viewer: role blocks, thinking collapsed, tool calls/args/results, long text collapsed, pagination; `arguments` may be a string preview.
- similar issues list; empty/error/404 states.
**Tests:** MSW detail/transcript; header/timeline/evidence/transcript pagination; 404.
**Commit:** `feat(web): issue detail, evidence and transcript viewer`.

---

### Task 7: 响应式、无障碍、错误与文档

**Files:** components/styles/tests, `web/src/app/ErrorBoundary.tsx`, docs
**Steps:**
- Responsive breakpoints 640/1024/1440; mobile tables→cards; sticky filter bar; app shell drawer.
- A11y: focus rings, labels, aria-live toast, semantic tables, reduced motion, no color-only status; axe smoke if Playwright available.
- Error boundary/offline banner/401 handling/empty states.
- Docs: docs/07 M3 bullet; README `npm ci && npm run build` / `npm run dev` + Vite proxy; docs/08 dev workflow.
**Commit:** `docs: record M3 SPA core and frontend workflow`.

---

### Task 8: 全量验证

- `cd web && npm ci && npm run typecheck && npm run lint && npm test && npm run build`
- `cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings`
- `cargo test -p nuwa -p shanhaijing`
- Start serve with temp DB; open `http://127.0.0.1:7402/` (built assets) → login → overview → issues → detail → transcript; three widths; light/dark; keyboard tab.
- Stop serve; confirm no listener.
- No push/tag/deploy.

## 自审记录

- M3 scope: login/shell/overview/issues/detail/transcript + visual system. Runners/events/settings/actions/cutover are M4–M6.
- No backend changes; all endpoints exist.
- No inline styles/scripts (strict CSP); theme init external.
- Tests are MSW/unit; manual browser verification required for layout/a11y.
- Known contract notes: `tool_call.arguments` may be a string preview; secrets never returned; actions deferred.
