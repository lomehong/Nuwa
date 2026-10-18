# Web Console M5 — Actions and Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add every owner write action and the full settings/system page, with CSRF, confirmations, pending states, audit feedback and restart protection.

**Architecture:** A small mutation layer (`features/issues/actions.ts`, `features/actions`) wraps the frozen M2c endpoints. One shared `IssueActionButton` renders state-appropriate reopen/requeue actions in the detail header, issue list cards/rows and the overview attention queue. The settings page is schema-driven: the API returns field metadata (including numeric bounds), the form tracks dirty values, saves only changed keys, and surfaces server field errors. Restart uses a 409-with-active-runs flow and polls `/health` + `/auth/me` after scheduling.

**Tech Stack:** React 19, TanStack Query mutations, Axum settings/actions API, Vitest + RTL + MSW, Playwright-core smoke script.

**Spec:** `docs/09-web-console-design.md` §8, §9.2, §10.3.1, §10.4, §10.5, §10.8, §12, §13, §14 M5, §15.2

## Global Constraints

- All mutations go through `apiFetch` (same-origin cookies + CSRF double-submit); never send credentials in bodies.
- Write feedback: pending disable, success toast, field errors inline, global error text, never a stack trace.
- Reopen is immediate (reversible); requeue, runner restart and service restart require confirmation explaining impact.
- A restart never proceeds past a 409 unless the operator explicitly checks “强制重启”.
- Settings secrets are write-only: input starts empty, empty means keep, only `set/last4` is displayed.
- No inline styles/scripts; touch controls ≥44px on coarse pointers.
- All work stays local on `main`; no push/tag/deploy.

---

## File structure

| File | Responsibility |
|---|---|
| `web/src/lib/api.ts` | `ApiError.body` for 409 payloads (`active_runs`) |
| `web/src/lib/queryInvalidation.ts` | Shared invalidation after writes |
| `web/src/features/issues/actions.ts` | reopen/requeue mutations + `issueActionFor` |
| `web/src/features/issues/IssueActionButton.tsx` | State-aware action + confirm + toasts |
| `web/src/features/actions/SyncButton.tsx` | GitHub reconcile action |
| `web/src/features/runners/RestartRunnerButton.tsx` | Unit restart with impact warning |
| `web/src/features/settings/types.ts` | Settings/system DTOs |
| `web/src/features/settings/queries.ts` | Settings/system queries + mutations |
| `web/src/features/settings/validation.ts` | Client-side field validation |
| `web/src/features/settings/SettingsPage.tsx` | Sections, dirty bar, save/test |
| `web/src/features/settings/SettingField.tsx` | One schema-driven field |
| `web/src/features/settings/RestartDialog.tsx` | Active-run guard + force |
| `web/src/app/router.tsx` | `/settings` route |
| `web/src/app/AppShell.tsx` | activate settings nav |
| `web/e2e/seed.py`, `web/e2e/smoke.mjs` | Local UAT harness |
| `crates/nuwa/src/api/settings.rs` | expose `min`/`max` in the schema |

---

### Task 1: Error body details and shared invalidation

**Files:**
- Modify: `web/src/lib/api.ts`
- Create: `web/src/lib/queryInvalidation.ts`
- Test: `web/src/lib/api.test.ts`
- Test: `web/src/lib/queryInvalidation.test.ts`

**Interfaces:**
- `ApiError` gains `readonly body?: unknown`.
- `invalidateIssueViews(queryClient, fingerprint?)` invalidates `overview`, `issues`, `issue`(fingerprint), `events`, `trends`, `runners`.
- `invalidateSettingsViews(queryClient)` invalidates `settings`, `system`, `events`.

- [ ] **Step 1: Write failing tests**

`api.test.ts`:

```ts
it("keeps extra 409 fields on the error body", async () => {
  server.use(
    http.post("/api/v1/settings/restart", () =>
      HttpResponse.json(
        {
          error: { code: "conflict", message: "active runs" },
          active_runs: [
            { id: "run-1", fingerprint: "fp-1", runner_id: "r1", repo: "a/b", phase: "repair", started_at_ms: 1 },
          ],
        },
        { status: 409 },
      ),
    ),
  );
  await expect(apiFetch("/settings/restart", { method: "POST", body: "{}" })).rejects.toMatchObject({
    status: 409,
    body: { active_runs: [{ id: "run-1" }] },
  });
});
```

`queryInvalidation.test.ts`:

```ts
it("invalidates every view affected by an issue write", async () => {
  const client = createTestQueryClient();
  const spy = vi.spyOn(client, "invalidateQueries");
  invalidateIssueViews(client, "fp-1");
  const keys = spy.mock.calls.map(([filters]) => filters?.queryKey);
  expect(keys).toEqual(
    expect.arrayContaining([
      ["overview"],
      ["issues"],
      ["issue", "fp-1"],
      ["events"],
      ["trends"],
      ["runners"],
    ]),
  );
});
```

- [ ] **Step 2: Run and watch fail**

`cd web && npx vitest run src/lib/api.test.ts src/lib/queryInvalidation.test.ts` → FAIL.

- [ ] **Step 3: Implement**

In `apiErrorFromResponse`, parse the JSON once:

```ts
let body: unknown;
if (raw) {
  try {
    body = JSON.parse(raw);
  } catch {
    body = undefined;
  }
}
if (body && isErrorEnvelope(body)) {
  code = body.error.code;
  message = body.error.message;
  fields = body.error.fields;
}
return new ApiError(response.status, code, message, { fields, retryAfterSeconds, body });
```

Add `readonly body?: unknown` and the constructor option.

`queryInvalidation.ts`:

```ts
import type { QueryClient } from "@tanstack/react-query";

export function invalidateIssueViews(queryClient: QueryClient, fingerprint?: string): void {
  void queryClient.invalidateQueries({ queryKey: ["overview"] });
  void queryClient.invalidateQueries({ queryKey: ["issues"] });
  if (fingerprint) {
    void queryClient.invalidateQueries({ queryKey: ["issue", fingerprint] });
  }
  void queryClient.invalidateQueries({ queryKey: ["events"] });
  void queryClient.invalidateQueries({ queryKey: ["trends"] });
  void queryClient.invalidateQueries({ queryKey: ["runners"] });
}

export function invalidateSettingsViews(queryClient: QueryClient): void {
  void queryClient.invalidateQueries({ queryKey: ["settings"] });
  void queryClient.invalidateQueries({ queryKey: ["system"] });
  void queryClient.invalidateQueries({ queryKey: ["events"] });
}
```

- [ ] **Step 4: Run tests**

`cd web && npx vitest run src/lib/api.test.ts src/lib/queryInvalidation.test.ts` → PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/api.ts web/src/lib/api.test.ts web/src/lib/queryInvalidation.ts web/src/lib/queryInvalidation.test.ts
git commit -m "feat(web): keep 409 bodies and centralize write invalidation"
```

---

### Task 2: Reopen / requeue actions

**Files:**
- Create: `web/src/features/issues/actions.ts`
- Create: `web/src/features/issues/IssueActionButton.tsx`
- Create: `web/src/features/issues/IssueActionButton.test.tsx`
- Modify: `web/src/features/issue-detail/IssueHeader.tsx`
- Modify: `web/src/features/issues/IssueCards.tsx`
- Modify: `web/src/features/issues/IssueTable.tsx`
- Modify: `web/src/features/overview/AttentionQueue.tsx`
- Test: `web/src/features/issue-detail/IssueDetailPage.test.tsx`
- Test: `web/src/features/issues/IssuesPage.test.tsx`

**Interfaces:**
- `issueActionFor(issue: { state: IssueState; stuck: boolean }): "reopen" | "requeue" | null`.
- `useReopenIssue()`, `useRequeueIssue()` mutations.
- `IssueActionButton` props `{ fingerprint: string; state: IssueState; stuck: boolean; size?: "sm" | "md"; className?: string }`.

- [ ] **Step 1: Write failing action tests**

```tsx
it("reopens a rejected issue immediately and invalidates views", async () => {
  let posted = false;
  server.use(
    mockMe(),
    mockIssueDetail({ issue: makeIssueDetail().issue }),
    mockRuns(),
    mockEvents(),
    http.post("/api/v1/issues/:fp/reopen", ({ request, params }) => {
      posted = params.fp === "fp-1";
      expect(request.headers.get("x-nuwa-csrf")).toBe("csrf-token");
      return new HttpResponse(null, { status: 204 });
    }),
  );
  document.cookie = "nuwa_csrf=csrf-token; path=/";
  renderApp("/issues/fp-1");

  fireEvent.click(await screen.findByRole("button", { name: "重新打开" }));
  await waitFor(() => expect(posted).toBe(true));
  expect(await screen.findByText("已重新入队")).toBeInTheDocument();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("asks for confirmation before requeueing a stuck issue", async () => {
  let posted = false;
  server.use(
    // ...detail with state "fixing", stuck true...
    http.post("/api/v1/issues/:fp/requeue", () => {
      posted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderApp("/issues/fp-1");

  fireEvent.click(await screen.findByRole("button", { name: "重排" }));
  expect(screen.getByRole("dialog", { name: "重排卡单问题？" })).toBeInTheDocument();
  expect(posted).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "确认重排" }));
  await waitFor(() => expect(posted).toBe(true));
});
```

Use the real `makeIssueDetail` factory and override `state`/`stuck` consistently.

- [ ] **Step 2: Run and watch fail**

`cd web && npx vitest run src/features/issues/IssueActionButton.test.tsx` → FAIL.

- [ ] **Step 3: Implement**

`actions.ts`:

```ts
import { useMutation, useQueryClient } from "@tanstack/react-query";
import type { IssueState } from "../../components/ui";
import { apiFetch } from "../../lib/api";
import { invalidateIssueViews } from "../../lib/queryInvalidation";

export type IssueAction = "reopen" | "requeue";

export function issueActionFor(issue: { state: IssueState; stuck: boolean }): IssueAction | null {
  if (issue.state === "escalated" || issue.state === "rejected") return "reopen";
  if ((issue.state === "triggered" || issue.state === "fixing") && issue.stuck) return "requeue";
  return null;
}

export function useReopenIssue(fingerprint: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      apiFetch<void>(`/issues/${encodeURIComponent(fingerprint)}/reopen`, {
        method: "POST",
        body: JSON.stringify({}),
      }),
    onSuccess: () => invalidateIssueViews(queryClient, fingerprint),
  });
}

export function useRequeueIssue(fingerprint: string, force = false) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      apiFetch<void>(`/issues/${encodeURIComponent(fingerprint)}/requeue`, {
        method: "POST",
        body: JSON.stringify(force ? { force: true } : {}),
      }),
    onSuccess: () => invalidateIssueViews(queryClient, fingerprint),
  });
}
```

`IssueActionButton.tsx`:
- Compute `action = issueActionFor({ state, stuck })`; return `null` when none.
- Reopen: `const mutation = useReopenIssue(fingerprint)`; on click `mutation.mutate()`; on success toast `{title:"已重新入队", description:"问题单已回到 new，等待 runner 领取。"}`; on error toast danger with `describeError` and `fields` if any.
- Requeue: hold `confirmOpen`; button opens `ConfirmDialog` titled `重排卡单问题？`, description `仅当问题单处于卡单状态时有效。重排会清除归属并重新入队，可能重复执行同一问题的修复。`, confirmLabel `确认重排`, danger; `onConfirm` calls mutation and closes on success.
- Buttons use `Button size={size}` and appropriate labels; disabled pending with `pending={mutation.isPending}`.

Integrations:
- `IssueHeader`: replace the current “M3 stays read-only” comment/action area with `<IssueActionButton ... size="sm" />` next to the state.
- `IssueCards`: add in the action row before “查看详情”.
- `IssueTable`: add to the operation cell before the detail arrow; icon variant not needed — use `size="sm"` with text.
- `AttentionQueue`: add to each card's footer; the queue item includes `state` and `stuck`.

- [ ] **Step 4: Run focused tests**

`cd web && npx vitest run src/features/issues src/features/issue-detail src/features/overview` → PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/issues web/src/features/issue-detail web/src/features/overview
git commit -m "feat(web): reopen and requeue issue actions"
```

---

### Task 3: GitHub sync action

**Files:**
- Create: `web/src/features/actions/SyncButton.tsx`
- Create: `web/src/features/actions/SyncButton.test.tsx`
- Modify: `web/src/features/issues/IssuesPage.tsx`

**Interfaces:**
- `POST /sync` → `{checked,merged,rejected,still_open,errors[]}`.
- `SyncButton` invalidates all data views.

- [ ] **Step 1: Write failing test**

```tsx
it("syncs and reports a summary, including per-issue errors", async () => {
  server.use(
    mockMe(),
    mockIssues(),
    http.post("/api/v1/sync", () =>
      HttpResponse.json({
        checked: 3,
        merged: 1,
        rejected: 0,
        still_open: 1,
        errors: ["fp-2 — GitHub 502"],
      }),
    ),
  );
  renderApp("/issues");
  fireEvent.click(await screen.findByRole("button", { name: "GitHub 对账" }));
  expect(await screen.findByText(/检查 3 · 合并 1 · 仍未合并 1/)).toBeInTheDocument();
  expect(screen.getByText("fp-2 — GitHub 502")).toBeInTheDocument();
});
```

- [ ] **Step 2: Run and watch fail**

`cd web && npx vitest run src/features/actions/SyncButton.test.tsx` → FAIL.

- [ ] **Step 3: Implement**

```tsx
export function SyncButton({ className }: { className?: string }) {
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const mutation = useMutation({
    mutationFn: () => apiFetch<SyncResponse>("/sync", { method: "POST", body: JSON.stringify({}) }),
    onSuccess: (result) => {
      invalidateIssueViews(queryClient);
      toast({
        title: "对账完成",
        description: `检查 ${result.checked} · 合并 ${result.merged} · 拒绝 ${result.rejected} · 仍未合并 ${result.still_open}` +
          (result.errors.length ? `；${result.errors.length} 个查询失败` : ""),
        variant: result.errors.length ? "warning" : "success",
      });
      for (const error of result.errors.slice(0, 3)) {
        toast({ title: "对账查询失败", description: error, variant: "danger", duration: 6000 });
      }
    },
    onError: (error) => {
      const message = error instanceof ApiError && error.status === 409
        ? "已有对账任务在运行，请稍后重试。"
        : describeError(error);
      toast({ title: "对账失败", description: message, variant: "danger" });
    },
  });
  return (
    <Button variant="secondary" size="sm" pending={mutation.isPending}
      onClick={() => mutation.mutate()} className={className}>
      <RefreshCw aria-hidden="true" className="size-3.5" />
      GitHub 对账
    </Button>
  );
}
```

Type `SyncResponse` in `features/actions/types.ts`; `errors` is `string[]` (server guarantee). Add the button to the issues page header aside.

- [ ] **Step 4: Run test**

`cd web && npx vitest run src/features/actions` → PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/actions web/src/features/issues/IssuesPage.tsx
git commit -m "feat(web): GitHub sync action"
```

---

### Task 4: Runner restart action

**Files:**
- Create: `web/src/features/runners/RestartRunnerButton.tsx`
- Create: `web/src/features/runners/RestartRunnerButton.test.tsx`
- Modify: `web/src/features/runners/RunnerCard.tsx`
- Modify: `web/src/features/runners/types.ts` (no change unless the card needs props)

**Interfaces:**
- `POST /runners/{id}/restart` → 202; errors 404/409/422/501.
- Confirmation text includes the current fingerprint when present; 501 shows the manual command from `ApiError.message`.

- [ ] **Step 1: Write failing test**

```tsx
it("warns about the active issue and surfaces a 501 manual command", async () => {
  server.use(
    mockMe(),
    mockRunners([makeRunner({ id: "runner-a", current_fingerprint: "fp-1" })]),
    http.post("/api/v1/runners/runner-a/restart", () =>
      HttpResponse.json(
        { error: { code: "not_supported", message: "systemd-run is unavailable; restart manually: systemctl restart nuwa-work" } },
        { status: 501 },
      ),
    ),
  );
  renderApp("/runners");
  fireEvent.click(await screen.findByRole("button", { name: "重启 unit" }));
  expect(screen.getByText(/当前正在修复 fp-1/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "确认重启" }));
  expect(await screen.findByText(/systemctl restart nuwa-work/)).toBeInTheDocument();
});
```

- [ ] **Step 2: Run and watch fail**

`cd web && npx vitest run src/features/runners/RestartRunnerButton.test.tsx` → FAIL.

- [ ] **Step 3: Implement**

- Button `variant="secondary" size="sm"` label `重启 unit`, disabled when `!runner.service_unit`.
- Confirm dialog title `重启 runner unit？`; description lists:
  - unit name,
  - current fingerprint warning when present,
  - “同一 unit 承载的多个 runner 会一起重启；建议一 runner 一 unit。”.
- Mutation `apiFetch<void>(\`/runners/${encodeURIComponent(id)}/restart\`, { method: "POST", body: "{}" })`.
- 202: toast success `重启已排程` + invalidate runners/overview.
- 409: `已有重启任务在排程，请稍后重试。`
- 422: show `fields.service_unit` in the toast.
- 501: toast danger with `ApiError.message` (contains the manual command), duration 8000.

Add the button to `RunnerCard` header actions. On coarse pointers both buttons remain ≥44px.

- [ ] **Step 4: Run tests**

`cd web && npx vitest run src/features/runners` → PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/runners
git commit -m "feat(web): restart runner unit with impact warning"
```

---

### Task 5: Settings data layer and schema bounds

**Files:**
- Modify: `crates/nuwa/src/api/settings.rs` (expose `min`/`max`)
- Test: `crates/nuwa/src/api/settings.rs` tests
- Create: `web/src/features/settings/types.ts`
- Create: `web/src/features/settings/queries.ts`
- Create: `web/src/features/settings/validation.ts`
- Create: `web/src/features/settings/validation.test.ts`
- Modify: `web/src/test/msw.ts`

**Interfaces:**
- Rust `SettingDto` gains `pub min: Option<u64>`, `pub max: Option<u64>` mapped from `SettingSpec`.
- Frontend types mirror the settings/system responses.
- `validateSettingValue(spec, raw): string | null` returns a human-safe Chinese error.

- [ ] **Step 1: Write the failing Rust test**

In the settings API test module:

```rust
#[tokio::test]
async fn schema_exposes_numeric_bounds() {
    let (_dir, app) = test_app();
    let value = json_body(get(&app, "/api/v1/settings").await).await;
    let schema = value["schema"].as_array().expect("schema");
    let interval = schema
        .iter()
        .find(|entry| entry["key"] == "NUWA_INTERVAL")
        .expect("NUWA_INTERVAL");
    assert_eq!(interval["min"], 1);
    assert_eq!(interval["max"], 86_400);
    let github = schema
        .iter()
        .find(|entry| entry["key"] == "NUWA_GITHUB_TOKEN")
        .expect("github");
    assert!(github["min"].is_null());
    assert!(github["max"].is_null());
}
```

- [ ] **Step 2: Run and watch fail**

`cargo test -p nuwa api::settings::tests::schema_exposes_numeric_bounds` → FAIL (unknown field / null).

- [ ] **Step 3: Implement Rust**

Add fields to `SettingDto` and `From<&SettingSpec>`:

```rust
pub min: Option<u64>,
pub max: Option<u64>,
// ...
min: spec.min,
max: spec.max,
```

- [ ] **Step 4: Write the failing frontend validation test**

```ts
describe("validateSettingValue", () => {
  const interval = spec({ key: "NUWA_INTERVAL", type: "int", min: 1, max: 86_400 });
  it("rejects non-integers and out-of-range numbers", () => {
    expect(validateSettingValue(interval, "abc")).toBe("请输入整数");
    expect(validateSettingValue(interval, "0")).toBe("必须在 1–86400 之间");
    expect(validateSettingValue(interval, "86401")).toBe("必须在 1–86400 之间");
    expect(validateSettingValue(interval, "30")).toBeNull();
  });
  it("validates enums and URLs", () => {
    expect(validateSettingValue(spec({ type: "enum", options: ["a", "b"] }), "c")).toBe("必须选择 a / b");
    expect(validateSettingValue(spec({ type: "url" }), "ftp://x")).toBe("必须是 http(s) URL");
    expect(validateSettingValue(spec({ type: "url" }), "")).toBeNull();
  });
});
```

- [ ] **Step 5: Implement frontend types/queries/validation**

`types.ts` mirrors `SettingsResponse`, `SettingDto` (including `min`/`max`), `SettingValueDto`, `DiffDto`, `LastChangeDto`, `SystemResponse`, `ServiceState`, and mutation request/response types.

`queries.ts`:
- `useSettingsQuery()` (`staleTime: 0`, `refetchOnWindowFocus: true`, no interval).
- `useSystemQuery()` (`staleTime: 30_000`).
- `useSaveSettingsMutation()` → `PUT /settings { values }`, invalidates settings/system/events on success.
- `useTestSettingsMutation()` → `POST /settings/test { target, values }` (no invalidation).
- `useRestartServicesMutation()` → `POST /settings/restart { force }`.

`validation.ts`:

```ts
export function validateSettingValue(spec: SettingDto, raw: string): string | null {
  if (raw === "") {
    return null; // non-secret empty follows the server default/clear rules
  }
  if (spec.type === "int") {
    if (!/^-?\d+$/.test(raw)) return "请输入整数";
    const value = Number(raw);
    if (((spec.min != null) && value < spec.min) || ((spec.max != null) && value > spec.max)) {
      return `必须在 ${spec.min ?? "…"}–${spec.max ?? "…"} 之间`;
    }
  }
  if (spec.type === "enum" && !spec.options.includes(raw)) {
    return `必须选择 ${spec.options.join(" / ")}`;
  }
  if (spec.type === "url" && !/^https?:\/\/\S+$/i.test(raw)) {
    return "必须是 http(s) URL";
  }
  return null;
}
```

Add `makeSettings`, `makeSystem`, `mockSettings`, `mockSystem` factories to `test/msw.ts`.

- [ ] **Step 6: Run both suites**

```bash
cargo test -p nuwa api::settings::tests
cd web && npx vitest run src/features/settings/validation.test.ts
```
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/nuwa/src/api/settings.rs web/src/features/settings web/src/test/msw.ts
git commit -m "feat(settings): expose numeric bounds and add settings data layer"
```

---

### Task 6: Settings page form

**Files:**
- Create: `web/src/features/settings/SettingsPage.tsx`
- Create: `web/src/features/settings/SettingField.tsx`
- Create: `web/src/features/settings/SettingsPage.test.tsx`
- Modify: `web/src/app/router.tsx`
- Modify: `web/src/app/AppShell.tsx`
- Modify: `web/src/features/issues/IssuesPage.tsx` (keep only if sync moved here)

**Interfaces:**
- Consumes `useSettingsQuery`, `useSystemQuery`, mutations, `validateSettingValue`.
- Produces `/settings` route and active nav.

- [ ] **Step 1: Write failing page tests**

```tsx
it("only sends changed keys and shows the restart notice", async () => {
  let body: unknown;
  server.use(
    mockMe(),
    mockSystem(),
    mockSettings(),
    http.put("/api/v1/settings", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ changed: ["NUWA_INTERVAL"], restart_required: true });
    }),
  );
  renderApp("/settings");
  const interval = await screen.findByLabelText("Poll interval");
  fireEvent.change(interval, { target: { value: "60" } });
  fireEvent.click(screen.getByRole("button", { name: "保存更改" }));

  await waitFor(() => expect(body).toEqual({ values: { NUWA_INTERVAL: 60 } }));
  expect(await screen.findByText("重启后生效")).toBeInTheDocument();
  expect(screen.getByText("NUWA_INTERVAL")).toBeInTheDocument();
});

it("never renders a stored secret value", async () => {
  server.use(mockMe(), mockSystem(), mockSettings());
  renderApp("/settings");
  expect(await screen.findByText("已设置 · 末 4 位 1234")).toBeInTheDocument();
  expect(screen.queryByDisplayValue("supersecret1234")).not.toBeInTheDocument();
});

it("shows server field errors next to the field", async () => {
  server.use(
    mockMe(), mockSystem(), mockSettings(),
    http.put("/api/v1/settings", () =>
      HttpResponse.json(
        { error: { code: "validation_failed", message: "invalid settings", fields: { NUWA_INTERVAL: "must be 1..86400" } } },
        { status: 422 },
      ),
    ),
  );
  renderApp("/settings");
  fireEvent.change(await screen.findByLabelText("Poll interval"), { target: { value: "60" } });
  fireEvent.click(screen.getByRole("button", { name: "保存更改" }));
  expect(await screen.findByText("must be 1..86400")).toBeInTheDocument();
});
```

- [ ] **Step 2: Run and watch fail**

`cd web && npx vitest run src/features/settings/SettingsPage.test.tsx` → FAIL.

- [ ] **Step 3: Implement**

`SettingsPage` layout:
- Header `设置` + description + dirty badge.
- Sections in order: `credentials` 凭据, `model` 模型, `runner` Runner, `service` 服务控制, `security` 安全, `ops` 运维, `advanced` 高级, then 关于.
- Each section is a `Card` with fields from `schema.filter(spec.group === group)`.
- Dirty state: `const [drafts, setDrafts] = useState<Record<string,string>>({})`; `dirty = Object.keys(drafts).length > 0`.
- Save bar fixed at the bottom of the content on `<lg` and inline on desktop:
  - `保存更改` pending; `放弃更改` resets drafts; text `N 项未保存`.
  - On save: client-validate every draft against its spec; if errors, set field errors and stop. Send `Object.fromEntries` with numbers parsed for int, booleans untouched (no bool type in schema), strings otherwise. Only dirty keys.
  - Success: toast `设置已保存`; clear drafts; if `restart_required`, show a persistent `Badge variant="warning"` “重启后生效” and the changed keys; call `invalidateSettingsViews`.
  - 422: map `ApiError.fields` into `fieldErrors`; toast danger.
- `SettingField`:
  - Secret: display `已设置 · 末 4 位 {last4}` or `未设置`; a `Button size="sm"` `更新` toggles a password `Input` with placeholder `留空保持不变`; `autocomplete="new-password"`.
  - Plain: `Input` for string/url/path, `Input type="number" min/max` for int, `Select` for enum, comma text for csv.
  - Read-only: render text, no input.
  - Show `spec.help`, `requires_restart` badge, `spec.scope`, and `effective_diff[key]` when present: `文件值 {file} · 运行中 {runtime}`.
  - Field error under the input.
- Service control:
  - `SystemResponse` version/build/uptime/db path/size/local mode.
  - Services list with `active` values; `n/a` styled as muted.
  - `SyncButton` and a `重启服务` button (Task 7).
- Advanced:
  - env path, permissions, `needs_migration` warning, `unknown_keys` names only, `last_changes` list (key/time/actor).
- About:
  - version, license `MIT`? Check repository license. Use `License` text from `Cargo.toml` workspace (`license.workspace = true`; inspect root). If unknown, show `见仓库 LICENSE`. Doc link `/docs`? Use GitHub repo external link `https://github.com/lomehong/nuwa` with `rel="noreferrer"` (known repo from AGENTS).
- Route/nav: add `/settings`, remove Soon.

- [ ] **Step 4: Run page tests**

`cd web && npx vitest run src/features/settings` → PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/settings web/src/app/router.tsx web/src/app/AppShell.tsx
git commit -m "feat(web): schema-driven settings page"
```

---

### Task 7: Settings test and restart orchestration

**Files:**
- Create: `web/src/features/settings/RestartDialog.tsx`
- Create: `web/src/features/settings/RestartDialog.test.tsx`
- Modify: `web/src/features/settings/SettingsPage.tsx`
- Create: `web/src/lib/health.ts`
- Test: `web/src/lib/health.test.ts`

**Interfaces:**
- `waitForServeRecovery(signal?): Promise<void>` polls `GET /health` every 1s up to 60s, then `GET /api/v1/auth/me` to confirm the session.
- Restart flow: 202 → set `restarting`; poll; on recovery `window.location.reload()`.
- 409 body `active_runs` → open the guard state showing runs; require the `强制重启` checkbox before enabling confirm.

- [ ] **Step 1: Write failing restart test**

```tsx
it("shows active runs on 409 and only enables force after confirmation", async () => {
  let body: unknown;
  server.use(
    mockMe(), mockSystem(), mockSettings(),
    http.post("/api/v1/settings/restart", async ({ request }) => {
      body = await request.json();
      if (!(body as { force?: boolean }).force) {
        return HttpResponse.json(
          {
            error: { code: "conflict", message: "active runs" },
            active_runs: [
              { id: "run-1", fingerprint: "fp-1", runner_id: "r1", repo: "a/b", phase: "repair", started_at_ms: Date.now() },
            ],
          },
          { status: 409 },
        );
      }
      return new HttpResponse(null, { status: 202 });
    }),
  );
  renderApp("/settings");
  fireEvent.click(await screen.findByRole("button", { name: "重启服务" }));
  fireEvent.click(screen.getByRole("button", { name: "确认重启" }));
  expect(await screen.findByText("fp-1")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "强制重启" })).toBeDisabled();
  fireEvent.click(screen.getByRole("checkbox", { name: /我了解这会中断正在进行的修复/ }));
  expect(screen.getByRole("button", { name: "强制重启" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "强制重启" }));
  await waitFor(() => expect(body).toEqual({ force: true }));
});
```

Mock `waitForServeRecovery` with `vi.mock` for the 202 path; assert it is called.

- [ ] **Step 2: Run and watch fail**

`cd web && npx vitest run src/features/settings/RestartDialog.test.tsx` → FAIL.

- [ ] **Step 3: Implement**

`health.ts`:

```ts
export async function waitForServeRecovery(timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch("/health", { cache: "no-store", credentials: "same-origin" });
      if (response.ok) {
        const me = await fetch("/api/v1/auth/me", { cache: "no-store", credentials: "include" });
        if (me.ok) return;
      }
    } catch {
      // still restarting
    }
    await new Promise((resolve) => window.setTimeout(resolve, 1_000));
  }
  throw new Error("服务在 60 秒内未恢复");
}
```

`RestartDialog` state machine: `idle → confirm → activeRuns → restarting → failed`.
- Initial confirm: title `重启服务？`, description lists serve + runner units from `/system`, warning that active runs are interrupted.
- On 409: store `active_runs`, render a list (fingerprint link to detail, runner, phase, age) and a checkbox. `强制重启` disabled until checked; on confirm POST `{force:true}`.
- On 202: close the dialog? Keep it open with `role="status"` “正在重启，等待服务恢复…” and a spinner; call `waitForServeRecovery`; on success `window.location.reload()`; on failure show the manual recovery hint + retry button.
- On 501: show `ApiError.message` (manual systemd command).
- Test buttons in the service section:
  - `测试 GitHub token` → mutation `{target:"github"}`; toast success/detail or field error.
  - `测试模型配置` → `{target:"llm", values: draftsFor([...llm keys])}`; toast detail.
  - Both pending and disabled appropriately.

- [ ] **Step 4: Run tests**

`cd web && npx vitest run src/features/settings src/lib/health.test.ts` → PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/features/settings web/src/lib/health.ts web/src/lib/health.test.ts
git commit -m "feat(web): settings probes and guarded restart"
```

---

### Task 8: UAT harness, docs and verification

**Files:**
- Create: `web/e2e/seed.py`
- Create: `web/e2e/smoke.mjs`
- Modify: `web/package.json` (add `test:e2e` and `playwright-core` dev dependency)
- Modify: `docs/07-open-items.md`
- Modify: `docs/08-onboarding.md`
- Modify: `web/README.md` (if present)

- [ ] **Step 1: Add the seed script**

`web/e2e/seed.py` (Python 3 stdlib) takes `--db` and `--bin`:
1. Run `<bin> status --db <db>` to create/migrate the DB.
2. Insert 8 issues across all states, 3 runners, 4 runs, events over 14 days, and one transcript under `--sessions` (absolute path). No credentials.
3. Print the issue fingerprints.

- [ ] **Step 2: Add the smoke script**

`web/e2e/smoke.mjs` uses `playwright-core` + system Chrome (`channel: "chrome"`), env `NUWA_E2E_BASE_URL`, `NUWA_E2E_TOKEN`, `NUWA_E2E_ENV_FILE`:
- Login.
- Overview → issues → detail → reopen a rejected issue → confirm the toast.
- Runners page → restart button opens the dialog, cancel (do not restart in E2E).
- Events page → audit preset.
- Settings → change `NUWA_INTERVAL` → save (no restart) → assert “保存更改” disabled and changed key shown.
- 390/1024/1440 screenshots under `web/e2e/artifacts/` (gitignored).
- Fail with a non-zero exit code on any console error or horizontal overflow.

Add `.gitignore` entry for `web/e2e/artifacts/`.

- [ ] **Step 3: Run E2E locally**

```bash
cd web && npm install
python e2e/seed.py --db ../.e2e.db --bin ~/.cargo/shared-target/debug/nuwa.exe --sessions ../.e2e-sessions
# start serve on 7403 with NUWA_UI_TOKEN=dummy and NUWA_ENV_FILE pointing at a temp env
NUWA_E2E_BASE_URL=http://127.0.0.1:7403 NUWA_E2E_TOKEN=dummy NUWA_E2E_ENV_FILE=/tmp/nuwa-e2e.env npm run test:e2e
```
Expected: all flows pass, artifacts written.

- [ ] **Step 4: Full verification**

```bash
cd web && npm run typecheck && npm run lint && npm test && npm run build
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p nuwa -p shanhaijing
```

- [ ] **Step 5: Docs**

- docs/07: M5 bullet with actions/settings/E2E evidence and deferred M6 items.
- docs/08: new console settings/actions instructions; remove “old UI only” wording where it conflicts (legacy removal is M6).

- [ ] **Step 6: Commit**

```bash
git add web/e2e web/package.json web/package-lock.json .gitignore docs/07-open-items.md docs/08-onboarding.md web/README.md
git commit -m "test(web): M5 E2E harness and docs"
```

---

## Self-review

- Spec coverage: reopen/requeue §8.5/§10.3/§10.4/§10.5 ✅; sync §9.2 ✅; runner restart §10.6 ✅; settings §10.8/§13 ✅; confirmations/toasts/field errors §12 ✅; E2E §15.2 ✅.
- Deferred: command palette, backoff, a11y/axe, legacy removal (M6).
- Type consistency: `IssueAction`, `SettingDto.min/max`, `ApiError.body`, and invalidation keys are used consistently across tasks.
