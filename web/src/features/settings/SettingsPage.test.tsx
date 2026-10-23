import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";

import {
  makeIssue,
  makeRepo,
  makeRunner,
  makeSettings,
  mockIssueDetail,
  mockIssues,
  mockMe,
  mockOverviewResources,
  mockRepos,
  mockRunners,
  mockSettings,
  mockSystem,
  mockTranscript,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
  // The repository section reuses the runner roster and the registry; every
  // test gets a baseline fleet and an empty registry unless it registers its
  // own handlers.
  server.use(mockRunners(), mockRepos([]));
});

type SettingsOverrides = Parameters<typeof mockSettings>[0];
type SystemOverrides = Parameters<typeof mockSystem>[0];

function signedIn(settings: SettingsOverrides = {}, system: SystemOverrides = {}) {
  server.use(
    mockMe(),
    ...mockOverviewResources(),
    mockSystem(system),
    mockSettings(settings),
  );
}

function bulkButton(name: string) {
  // Desktop and mobile section navs both expose the bulk controls; jsdom sees
  // both, so target the first (desktop) one.
  return screen.getAllByRole("button", { name })[0];
}

function saveBar() {
  return screen.getByTestId("settings-save-bar");
}

function saveButton() {
  return within(saveBar()).getByRole("button", { name: "保存更改" });
}

function discardButton() {
  return within(saveBar()).getByRole("button", { name: "放弃更改" });
}

function toggleSection(name: string) {
  fireEvent.click(screen.getByRole("button", { name }));
}

function shellNavLink(name: string) {
  const nav = screen.getByRole("navigation", { name: "主导航" });
  return within(nav).getByRole("link", { name });
}

const LEAVE_DIALOG = "放弃未保存的更改？";

describe("SettingsPage", () => {
  it("only sends changed keys and shows the restart notice", async () => {
    let body: unknown;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
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
    fireEvent.click(saveButton());

    await waitFor(() => expect(body).toEqual({ values: { NUWA_INTERVAL: 60 } }));
    expect(await screen.findByText("重启后生效")).toBeInTheDocument();
    expect(screen.getByText("NUWA_INTERVAL")).toBeInTheDocument();
    expect(screen.getByText("设置已保存")).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-restart-runner")).toBeInTheDocument();
  });

  it("never renders a stored secret value", async () => {
    signedIn();
    renderApp("/settings");

    expect(await screen.findByText("已设置 · 末 4 位 1234")).toBeInTheDocument();
    expect(screen.queryByDisplayValue("supersecret1234")).not.toBeInTheDocument();
    expect(screen.queryByDisplayValue(/1234/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "更新 GitHub token" }));
    const secretInput = screen.getByPlaceholderText("留空保持不变");
    expect(secretInput).toHaveAttribute("type", "password");
    expect(secretInput).toHaveAttribute("autocomplete", "new-password");
    expect(secretInput).toHaveValue("");
    const cancel = screen.getByRole("button", { name: "取消更新 GitHub token" });
    expect(cancel).toHaveTextContent("取消");
    expect(cancel).toHaveAttribute("aria-expanded", "true");
  });

  it("shows server field errors next to the field", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.put("/api/v1/settings", () =>
        HttpResponse.json(
          {
            error: {
              code: "validation_failed",
              message: "invalid settings",
              fields: { NUWA_INTERVAL: "must be 1..86400" },
            },
          },
          { status: 422 },
        ),
      ),
    );
    renderApp("/settings");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });
    fireEvent.click(saveButton());

    expect(await screen.findByText("must be 1..86400")).toBeInTheDocument();
    expect(screen.getByText("保存失败")).toBeInTheDocument();
  });

  it("blocks an out-of-range draft before the PUT", async () => {
    let put = false;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.put("/api/v1/settings", () => {
        put = true;
        return HttpResponse.json({ changed: [], restart_required: false });
      }),
    );
    renderApp("/settings");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "0" },
    });
    fireEvent.click(saveButton());

    expect(await screen.findByText("必须在 1–86400 之间")).toBeInTheDocument();
    expect(put).toBe(false);
  });

  it("tracks dirty drafts and discards them", async () => {
    signedIn();
    renderApp("/settings");
    const interval = await screen.findByLabelText("Poll interval");

    fireEvent.change(interval, { target: { value: "60" } });
    const bar = saveBar();
    expect(within(bar).getByText("1 项未保存")).toBeInTheDocument();
    const headerActions = screen.getByTestId("settings-header-actions");
    expect(within(headerActions).getByText("1 项未保存")).toBeInTheDocument();
    expect(within(headerActions).getByRole("button", { name: "保存更改" })).toBeInTheDocument();
    expect(screen.getByTestId("settings-nav-dirty-runner")).toBeInTheDocument();
    expect(bar).toHaveClass("bottom-14", "z-20", "lg:static");
    expect(bar).not.toHaveClass("bottom-0");

    fireEvent.click(discardButton());
    expect(screen.queryByText("1 项未保存")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-header-actions")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-nav-dirty-runner")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(30);
  });

  it("renders every schema group plus service, advanced and about", async () => {
    signedIn();
    renderApp("/settings");

    await screen.findByRole("heading", { name: "凭据" });
    for (const title of ["模型", "Runner", "服务控制", "安全", "运维", "高级", "关于"]) {
      expect(screen.getByRole("heading", { name: title })).toBeInTheDocument();
    }
    // Hidden panels stay mounted; expand them before asserting their content.
    fireEvent.click(bulkButton("展开全部"));
    expect(screen.getAllByText("0.2.0").length).toBeGreaterThan(0);
    expect(screen.getByText("MIT")).toBeInTheDocument();
    const repo = screen.getByRole("link", { name: /github\.com\/lomehong\/nuwa/ });
    expect(repo).toHaveAttribute("rel", "noreferrer");
    expect(screen.getByText("n/a")).toHaveClass("text-text-muted");
    // Plain fields expose where their current value comes from.
    expect(screen.getAllByText("来源 env 文件").length).toBeGreaterThan(0);
    expect(screen.getAllByText("来源 默认值").length).toBeGreaterThan(0);
  });

  it("renders the read-only env path as text, not an input", async () => {
    signedIn();
    renderApp("/settings");

    expect(await screen.findByText("Env file")).toBeInTheDocument();
    expect(screen.queryByLabelText("Env file")).not.toBeInTheDocument();
    expect(screen.getByText("/opt/nuwa/nuwa.env")).toBeInTheDocument();
    expect(screen.getByText("LEGACY_KEY")).toBeInTheDocument();
    expect(screen.getByText("NUWA_MAX_TURNS")).toBeInTheDocument();
  });

  it("warns about restart-pending and needs-migration state", async () => {
    signedIn({ restart_pending: true, needs_migration: true });
    renderApp("/settings");

    expect(await screen.findByText("重启后生效")).toBeInTheDocument();
    expect(screen.getByText(/检测到旧格式键名/)).toBeInTheDocument();
    expect(screen.getByText(/文件值 30 · 运行中 60/)).toBeInTheDocument();
  });

  it("warns when the console runs in local mode", async () => {
    server.use(
      mockMe({ local_mode: true, expires_at_ms: null }),
      ...mockOverviewResources(),
      mockSystem({ local_mode: true }),
      mockSettings(),
    );
    renderApp("/settings");

    expect(
      await screen.findByText(/任何能访问该端口的客户端都可修改设置/),
    ).toBeInTheDocument();
  });

  it("guards in-app navigation while drafts are unsaved", async () => {
    signedIn();
    renderApp("/settings");
    const interval = await screen.findByLabelText("Poll interval");
    fireEvent.change(interval, { target: { value: "60" } });

    fireEvent.click(shellNavLink("总览"));
    const dialog = await screen.findByRole("dialog", { name: LEAVE_DIALOG });
    expect(within(dialog).getByRole("button", { name: "留在本页" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "放弃并离开" })).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "留在本页" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("heading", { level: 2, name: "设置" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);

    fireEvent.click(shellNavLink("总览"));
    fireEvent.click(await screen.findByRole("button", { name: "放弃并离开" }));
    expect(await screen.findByRole("heading", { level: 1, name: "总览" })).toBeInTheDocument();
  });

  it("navigates without a dialog when nothing is dirty", async () => {
    signedIn();
    renderApp("/settings");
    await screen.findByLabelText("Poll interval");

    fireEvent.click(shellNavLink("总览"));

    expect(await screen.findByRole("heading", { level: 1, name: "总览" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument();
  });

  it("closes the drawer behind the guard and unlocks scroll after cancel", async () => {
    signedIn();
    renderApp("/settings");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    fireEvent.click(screen.getByRole("button", { name: "打开导航菜单" }));
    const drawer = screen.getByRole("dialog", { name: "导航菜单" });
    fireEvent.click(within(drawer).getByRole("link", { name: "总览" }));

    expect(await screen.findByRole("dialog", { name: LEAVE_DIALOG })).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "导航菜单" })).not.toBeInTheDocument();
    expect(document.body).toHaveClass("nuwa-scroll-lock");

    fireEvent.click(screen.getByRole("button", { name: "留在本页" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument(),
    );
    expect(document.body).not.toHaveClass("nuwa-scroll-lock");
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);
  });

  it("ignores a same-route navigation that differs only by a trailing slash", async () => {
    signedIn();
    const { router } = renderApp("/settings");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    // A programmatic navigation to the same route must not trip the guard.
    await act(async () => {
      await router.navigate("/settings/");
    });

    expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument();
    expect(within(saveBar()).getByText("1 项未保存")).toBeInTheDocument();
  });

  it("ignores a same-route navigation whose query parameters are reordered", async () => {
    signedIn();
    const { router } = renderApp("/settings?a=1&b=2");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    await act(async () => {
      await router.navigate("/settings?b=2&a=1");
    });

    expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument();
    expect(within(saveBar()).getByText("1 项未保存")).toBeInTheDocument();
  });

  // WHY: the command palette leaves the page through navigate() (a programmatic
  // PUSH), which the old anchor-click guard could not see. The data-router
  // blocker must intercept it and ask before dropping the drafts.
  it("guards the command palette's programmatic navigation while dirty", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      mockIssues({
        data: [makeIssue({ fingerprint: "fp-palette", title: "Palette target panic" })],
      }),
    );
    renderApp("/settings");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    const palette = await screen.findByRole("dialog", { name: "搜索问题单" });
    const input = within(palette).getByRole("searchbox", { name: "搜索问题单" });
    fireEvent.change(input, { target: { value: "panic" } });
    await within(palette).findByRole("link", { name: /Palette target panic/ });
    fireEvent.keyDown(input, { key: "Enter" });

    // The palette closed and the leave guard opened instead of navigating away.
    const dialog = await screen.findByRole("dialog", { name: LEAVE_DIALOG });
    fireEvent.click(within(dialog).getByRole("button", { name: "留在本页" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("heading", { level: 2, name: "设置" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);
  });

  // WHY: the browser back button (POP) also dropped drafts under the old guard.
  // The blocker must intercept POP, and cancel must restore the settings page
  // with its drafts while confirm completes the back navigation.
  it("guards browser back (POP) while drafts are unsaved", async () => {
    signedIn();
    server.use(mockIssues());
    const { router } = renderApp("/");
    // Push settings onto the stack so there is a back target, then dirty it.
    await act(async () => {
      await router.navigate("/settings");
    });
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    await act(async () => {
      await router.navigate(-1);
    });

    const dialog = await screen.findByRole("dialog", { name: LEAVE_DIALOG });
    fireEvent.click(within(dialog).getByRole("button", { name: "留在本页" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("heading", { level: 2, name: "设置" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);

    // Back again, then confirm: the POP completes and we land on the overview.
    await act(async () => {
      await router.navigate(-1);
    });
    fireEvent.click(await screen.findByRole("button", { name: "放弃并离开" }));
    expect(
      await screen.findByRole("heading", { level: 2, name: "总览" }),
    ).toBeInTheDocument();
  });

  it("warns that a console token change ends the session on restart", async () => {
    const base = makeSettings();
    const secretSpec = base.schema.find((spec) => spec.secret)!;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings({
        schema: [
          ...base.schema,
          { ...secretSpec, key: "NUWA_UI_TOKEN", label: "Console token" },
        ],
      }),
      http.put("/api/v1/settings", () =>
        HttpResponse.json({ changed: ["NUWA_UI_TOKEN"], restart_required: true }),
      ),
    );
    renderApp("/settings");
    fireEvent.click(await screen.findByRole("button", { name: "更新 Console token" }));
    fireEvent.change(screen.getByPlaceholderText("留空保持不变"), {
      target: { value: "new-console-token" },
    });
    fireEvent.click(saveButton());

    expect(
      await screen.findByText("更改控制台令牌后，重启会使当前会话失效，需要重新登录。"),
    ).toBeInTheDocument();
  });

  it("keeps drafts edited while the save request is pending", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.put("/api/v1/settings", async () => {
        await gate;
        return HttpResponse.json({ changed: ["NUWA_INTERVAL"], restart_required: false });
      }),
    );
    renderApp("/settings");
    const interval = await screen.findByLabelText("Poll interval");
    fireEvent.change(interval, { target: { value: "60" } });
    fireEvent.click(saveButton());
    // Retype while the PUT is still in flight: this draft was not submitted.
    fireEvent.change(interval, { target: { value: "90" } });
    release();

    expect(await screen.findByText("设置已保存")).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(90);
    expect(within(saveBar()).getByText("1 项未保存")).toBeInTheDocument();
  });

  it("probes GitHub with no values and the LLM with only non-empty drafts", async () => {
    const bodies: Array<Record<string, unknown>> = [];
    const csrf: Array<string | null> = [];
    const base = makeSettings();
    const providerSpec = base.schema.find((spec) => spec.key === "NUWA_PROVIDER")!;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings({
        schema: [
          ...base.schema,
          {
            ...providerSpec,
            key: "NUWA_MODEL",
            label: "Model",
            type: "string",
            options: [],
            default: "",
          },
          {
            ...providerSpec,
            key: "NUWA_BASE_URL",
            label: "Base URL",
            type: "url",
            options: [],
            default: "",
          },
        ],
        values: {
          ...base.values,
          NUWA_MODEL: { value: "", source: "default" },
          NUWA_BASE_URL: { value: "", source: "default" },
        },
      }),
      http.post("/api/v1/settings/test", async ({ request }) => {
        csrf.push(request.headers.get("x-nuwa-csrf"));
        const body = (await request.json()) as Record<string, unknown>;
        bodies.push(body);
        return HttpResponse.json({
          ok: true,
          detail: body.target === "github" ? "token accepted; 1/2" : "zai model responded",
        });
      }),
    );
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "测试 GitHub token" }));
    await waitFor(() => expect(bodies[0]).toEqual({ target: "github" }));
    expect(await screen.findByText("token accepted; 1/2")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("LLM Provider"), { target: { value: "zai" } });
    fireEvent.change(screen.getByLabelText("Model"), {
      target: { value: "glm-5.3-flash" },
    });
    fireEvent.click(screen.getByRole("button", { name: "测试模型配置" }));

    await waitFor(() =>
      expect(bodies[1]).toEqual({
        target: "llm",
        values: { NUWA_PROVIDER: "zai", NUWA_MODEL: "glm-5.3-flash" },
      }),
    );
    expect(await screen.findByText("zai model responded")).toBeInTheDocument();
    expect(csrf).toEqual(["csrf-token", "csrf-token"]);
  });

  it("probes GitHub with the typed token instead of the stored one", async () => {
    const bodies: Array<Record<string, unknown>> = [];
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.post("/api/v1/settings/test", async ({ request }) => {
        bodies.push((await request.json()) as Record<string, unknown>);
        return HttpResponse.json({ ok: true, detail: "token accepted; 2/3" });
      }),
    );
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "更新 GitHub token" }));
    fireEvent.change(screen.getByPlaceholderText("留空保持不变"), {
      target: { value: "ghp_candidate_token" },
    });
    fireEvent.click(screen.getByRole("button", { name: "测试 GitHub token" }));

    await waitFor(() =>
      expect(bodies[0]).toEqual({
        target: "github",
        values: { NUWA_GITHUB_TOKEN: "ghp_candidate_token" },
      }),
    );
    // The stored secret (only its last four characters are ever echoed) is
    // never sent; the probe uses exactly what the operator typed.
    expect(JSON.stringify(bodies[0])).not.toContain("1234");
  });

  it("disables restarting while drafts are unsaved", async () => {
    signedIn();
    renderApp("/settings");
    const restart = await screen.findByRole("button", { name: "重启服务" });
    expect(restart).toBeEnabled();

    fireEvent.change(screen.getByLabelText("Poll interval"), {
      target: { value: "60" },
    });
    expect(restart).toBeDisabled();
    expect(
      screen.getByText("请先保存或放弃未保存的更改"),
    ).toBeInTheDocument();

    fireEvent.click(discardButton());
    expect(restart).toBeEnabled();
    expect(
      screen.queryByText("请先保存或放弃未保存的更改"),
    ).not.toBeInTheDocument();
  });

  it("surfaces a probe 422 as a danger toast with the field message", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.post("/api/v1/settings/test", () =>
        HttpResponse.json(
          {
            error: {
              code: "validation_failed",
              message: "invalid settings",
              fields: { NUWA_GITHUB_TOKEN: "token is required" },
            },
          },
          { status: 422 },
        ),
      ),
    );
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "测试 GitHub token" }));

    expect(await screen.findByText("GitHub token 测试失败")).toBeInTheDocument();
    expect(
      screen.getByText("NUWA_GITHUB_TOKEN：token is required"),
    ).toBeInTheDocument();
  });

  it("opens the guarded restart dialog with the configured units", async () => {
    signedIn();
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "重启服务" }));

    const dialog = await screen.findByRole("dialog", { name: "重启服务？" });
    expect(within(dialog).getByText(/nuwa-serve、nuwa-work/)).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "确认重启" })).toBeInTheDocument();
  });

  it("clears a probe field error once a later probe succeeds", async () => {
    let calls = 0;
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      http.post("/api/v1/settings/test", () => {
        calls += 1;
        if (calls === 1) {
          return HttpResponse.json(
            {
              error: {
                code: "validation_failed",
                message: "invalid settings",
                fields: { NUWA_INTERVAL: "must be 1..86400" },
              },
            },
            { status: 422 },
          );
        }
        return HttpResponse.json({ ok: true, detail: "model responded" });
      }),
    );
    renderApp("/settings");

    const probe = await screen.findByRole("button", { name: "测试模型配置" });
    fireEvent.click(probe);
    expect(await screen.findByText("must be 1..86400")).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveAttribute("aria-invalid", "true");

    fireEvent.click(probe);
    expect(await screen.findByText("model responded")).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByText("must be 1..86400")).not.toBeInTheDocument(),
    );
    expect(screen.getByLabelText("Poll interval")).not.toHaveAttribute("aria-invalid");
  });

  it("lets an active-run link in the restart dialog navigate despite dirty drafts", async () => {
    server.use(
      mockMe(),
      ...mockOverviewResources(),
      mockSystem(),
      mockSettings(),
      mockIssueDetail(),
      mockTranscript(),
      http.post("/api/v1/settings/restart", () =>
        HttpResponse.json(
          {
            error: { code: "conflict", message: "active runs" },
            active_runs: [
              {
                id: "run-1",
                fingerprint: "fp-1",
                runner_id: "r1",
                repo: "a/b",
                phase: "repair",
                started_at_ms: Date.now(),
              },
            ],
          },
          { status: 409 },
        ),
      ),
    );
    renderApp("/settings");
    fireEvent.click(await screen.findByRole("button", { name: "重启服务" }));
    fireEvent.click(await screen.findByRole("button", { name: "确认重启" }));
    await screen.findByText("fp-1");
    // The restart button is disabled while drafts exist, so a dirty state can
    // only appear after the dialog opened; the page-owned dialog must still
    // let its active-run link navigate instead of stacking the leave guard.
    fireEvent.change(screen.getByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    fireEvent.click(await screen.findByRole("link", { name: "fp-1" }));

    expect(
      await screen.findByRole("heading", { level: 2, name: "Default issue" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "有正在进行的修复" })).not.toBeInTheDocument();
  });

  it("groups runners into a read-only repository view", async () => {
    server.use(
      mockRunners([
        makeRunner({
          id: "runner-a",
          repo: "example.com/app",
          repo_path: "/srv/app",
          prs_today: 1,
          daily_pr_limit: 5,
          current_fingerprint: "fp-alpha",
          last_error: "checkout dirty",
        }),
        makeRunner({
          id: "runner-b",
          repo: "example.com/app",
          repo_path: "/srv/app",
          heartbeat_at_ms: Date.now() - 600_000,
          offline: true,
          // `prs_today` is repo-scoped: both runners report the same count.
          prs_today: 1,
          daily_pr_limit: 5,
        }),
        makeRunner({ id: "runner-c", repo: "example.com/other", repo_path: "/srv/other" }),
      ]),
    );
    signedIn();
    renderApp("/settings");

    expect(await screen.findByText("example.com/app")).toBeInTheDocument();
    const group = screen.getByTestId("repo-group-example.com/app");
    expect(within(group).getByText("1/2 在线")).toBeInTheDocument();
    expect(within(group).getByText("/srv/app")).toBeInTheDocument();
    expect(within(group).getByText("go test ./...")).toBeInTheDocument();
    expect(within(group).getByText("3 个")).toBeInTheDocument();
    expect(within(group).getByText("1/5")).toBeInTheDocument();
    expect(within(group).getByText("checkout dirty")).toBeInTheDocument();
    expect(
      within(group).getByRole("link", { name: "查看当前单：fp-alpha" }),
    ).toHaveAttribute("href", "/issues/fp-alpha");
    expect(screen.getByTestId("repo-group-example.com/other")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "查看运行器" })).toHaveAttribute(
      "href",
      "/runners",
    );
  });

  it("merges the registry with runner groups", async () => {
    server.use(
      mockRepos([
        makeRepo({
          repo: "example.com/app",
          name: "Acme App",
          url: "https://github.com/acme/app",
          description: "primary",
        }),
        makeRepo({
          repo: "example.com/quiet",
          name: "Quiet Service",
          url: "ssh://git@example.com/quiet.git",
        }),
      ]),
      mockRunners([
        makeRunner({ id: "runner-a", repo: "example.com/app", repo_path: "/srv/app" }),
        makeRunner({ id: "runner-c", repo: "example.com/other", repo_path: "/srv/other" }),
      ]),
    );
    signedIn();
    renderApp("/settings");

    const registered = await screen.findByTestId("repo-group-example.com/app");
    expect(within(registered).getByText("Acme App")).toBeInTheDocument();
    expect(within(registered).getByText("example.com/app")).toBeInTheDocument();
    expect(within(registered).getByText("https://github.com/acme/app")).toBeInTheDocument();
    expect(within(registered).getByText("primary")).toBeInTheDocument();
    // Repository content stays read-only: the card has no editable field.
    expect(within(registered).queryByRole("textbox")).not.toBeInTheDocument();
    expect(within(registered).getByText("go test ./...")).toBeInTheDocument();
    expect(within(registered).getByText("3 个")).toBeInTheDocument();
    expect(within(registered).getByText("/srv/app")).toBeInTheDocument();
    expect(
      within(registered).getByRole("button", { name: "编辑 Acme App" }),
    ).toBeInTheDocument();

    const quiet = screen.getByTestId("repo-group-example.com/quiet");
    expect(within(quiet).getByText("Quiet Service")).toBeInTheDocument();
    expect(within(quiet).getByText("暂无 runner 上报")).toBeInTheDocument();
    expect(
      within(quiet).queryByRole("button", { name: "登记" }),
    ).not.toBeInTheDocument();

    const unregistered = screen.getByTestId("repo-group-example.com/other");
    expect(within(unregistered).getByText("未登记")).toBeInTheDocument();
    const register = within(unregistered).getByRole("button", { name: "登记" });
    expect(within(unregistered).getByText("/srv/other")).toBeInTheDocument();

    fireEvent.click(register);
    const dialog = await screen.findByRole("dialog", { name: "添加仓库" });
    expect(within(dialog).getByLabelText("仓库标识")).toHaveValue("example.com/other");
    expect(within(dialog).getByLabelText("名称")).toHaveValue("example.com/other");
  });

  it("creates a repository and refreshes the registry list", async () => {
    const created = makeRepo({
      repo: "example.com/new",
      name: "New App",
      url: "https://example.com/new",
      description: "brand new",
    });
    let posted: unknown;
    let csrf: string | null = null;
    let listCalls = 0;
    server.use(
      http.get("/api/v1/repos", () => {
        listCalls += 1;
        return HttpResponse.json({ data: listCalls > 1 ? [created] : [] });
      }),
      http.post("/api/v1/repos", async ({ request }) => {
        csrf = request.headers.get("x-nuwa-csrf");
        posted = await request.json();
        return HttpResponse.json({ data: created }, { status: 201 });
      }),
    );
    signedIn();
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "添加仓库" }));
    const dialog = await screen.findByRole("dialog", { name: "添加仓库" });
    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "New App" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库标识"), {
      target: { value: "example.com/new" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库地址"), {
      target: { value: "https://example.com/new" },
    });
    fireEvent.change(within(dialog).getByLabelText("描述（可选）"), {
      target: { value: "brand new" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(posted).toEqual({
        repo: "example.com/new",
        name: "New App",
        url: "https://example.com/new",
        description: "brand new",
      }),
    );
    expect(csrf).toBe("csrf-token");
    expect(await screen.findByText("仓库已添加")).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "添加仓库" })).not.toBeInTheDocument(),
    );
    // The mutation invalidates ["repos"], so the list refetches and shows it.
    expect(await screen.findByTestId("repo-group-example.com/new")).toBeInTheDocument();
    expect(listCalls).toBeGreaterThan(1);
  });

  it("validates the repository form before sending", async () => {
    let posted = false;
    server.use(
      http.post("/api/v1/repos", () => {
        posted = true;
        return HttpResponse.json({ data: makeRepo() }, { status: 201 });
      }),
    );
    signedIn();
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "添加仓库" }));
    const dialog = await screen.findByRole("dialog", { name: "添加仓库" });
    fireEvent.click(within(dialog).getByRole("button", { name: "添加" }));

    expect(await within(dialog).findByText("请填写名称")).toBeInTheDocument();
    expect(within(dialog).getByText("请填写仓库标识")).toBeInTheDocument();
    expect(within(dialog).getByText("请填写仓库地址")).toBeInTheDocument();
    expect(posted).toBe(false);

    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "New App" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库标识"), {
      target: { value: "example.com/new" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库地址"), {
      target: { value: "ftp://example.com/new" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "添加" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "请填写 HTTP/HTTPS、SSH、git 或无协议仓库地址",
    );
    expect(posted).toBe(false);

    // SSH (scp-style) addresses are accepted and submit.
    fireEvent.change(within(dialog).getByLabelText("仓库地址"), {
      target: { value: "git@example.com:new.git" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "添加" }));
    await waitFor(() => expect(posted).toBe(true));
  });

  it("edits a registered repository on an encoded path", async () => {
    const repo = makeRepo({
      repo: "example.com/app",
      name: "Acme App",
      url: "https://example.com/app",
    });
    let putUrl = "";
    let body: unknown;
    server.use(
      mockRepos([repo]),
      http.put("/api/v1/repos/:repo", async ({ request }) => {
        putUrl = new URL(request.url).pathname;
        body = await request.json();
        return HttpResponse.json({ data: { ...repo, name: "Acme Console" } });
      }),
    );
    signedIn();
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "编辑 Acme App" }));
    const dialog = await screen.findByRole("dialog", { name: "编辑仓库" });
    // Identity is path-owned: shown as text, never as an editable field.
    expect(within(dialog).queryByLabelText("仓库标识")).not.toBeInTheDocument();
    expect(within(dialog).getByText("example.com/app")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("名称")).toHaveValue("Acme App");

    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "Acme Console" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(body).toEqual({
        name: "Acme Console",
        url: "https://example.com/app",
        description: "",
      }),
    );
    expect(putUrl).toBe("/api/v1/repos/example.com%2Fapp");
    expect(await screen.findByText("仓库已更新")).toBeInTheDocument();
  });

  it("removes a registration only after confirmation", async () => {
    const repo = makeRepo({ repo: "example.com/app", name: "Acme App" });
    const deletes: string[] = [];
    server.use(
      mockRepos([repo]),
      http.delete("/api/v1/repos/:repo", ({ request }) => {
        deletes.push(new URL(request.url).pathname);
        return new HttpResponse(null, { status: 204 });
      }),
    );
    signedIn();
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "删除登记：Acme App" }));
    const dialog = await screen.findByRole("dialog", { name: "移除仓库登记？" });
    expect(within(dialog).getByText(/不会停止 runner/)).toBeInTheDocument();
    expect(within(dialog).getByText(/nuwa\.repo\.json/)).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "移除仓库登记？" }),
      ).not.toBeInTheDocument(),
    );
    expect(deletes).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "删除登记：Acme App" }));
    fireEvent.click(await screen.findByRole("button", { name: "移除登记" }));
    await waitFor(() => expect(deletes).toEqual(["/api/v1/repos/example.com%2Fapp"]));
    expect(await screen.findByText("已移除仓库登记")).toBeInTheDocument();
  });

  it("maps repository 422 fields inline", async () => {
    server.use(
      http.post("/api/v1/repos", () =>
        HttpResponse.json(
          {
            error: {
              code: "validation_failed",
              message: "invalid repository",
              fields: {
                repo: "identity is already taken",
                url: "must use https or ssh",
              },
            },
          },
          { status: 422 },
        ),
      ),
    );
    signedIn();
    renderApp("/settings");

    fireEvent.click(await screen.findByRole("button", { name: "添加仓库" }));
    const dialog = await screen.findByRole("dialog", { name: "添加仓库" });
    fireEvent.change(within(dialog).getByLabelText("名称"), {
      target: { value: "Acme App" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库标识"), {
      target: { value: "example.com/app" },
    });
    fireEvent.change(within(dialog).getByLabelText("仓库地址"), {
      target: { value: "https://example.com/app" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "添加" }));

    expect(
      await within(dialog).findByText("identity is already taken"),
    ).toBeInTheDocument();
    expect(within(dialog).getByText("must use https or ssh")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("仓库标识")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    expect(screen.getByText("添加失败")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "添加" })).toBeEnabled();
  });

  it("shows an empty repository note when no runner reported a repo", async () => {
    server.use(mockRunners([]));
    signedIn();
    renderApp("/settings");

    expect(await screen.findByText("还没有仓库注册")).toBeInTheDocument();
    expect(screen.getByText(/仓库来自运行器心跳上报/)).toBeInTheDocument();
  });

  it("offers a retry when the runner roster fails to load", async () => {
    server.use(
      http.get("/api/v1/runners", () =>
        HttpResponse.json(
          { error: { code: "internal", message: "runner store unavailable" } },
          { status: 500 },
        ),
      ),
    );
    signedIn();
    renderApp("/settings");

    expect(await screen.findByText("无法加载仓库")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("anchors the section nav and collapses sections by default", async () => {
    signedIn();
    renderApp("/settings");
    await screen.findByRole("heading", { name: "凭据" });

    const nav = screen.getByRole("navigation", { name: "设置分区" });
    const ids: Array<[string, string]> = [
      ["凭据", "settings-credentials"],
      ["模型", "settings-model"],
      ["Runner", "settings-runner"],
      ["仓库", "settings-repo"],
      ["服务控制", "settings-service"],
      ["安全", "settings-security"],
      ["运维", "settings-ops"],
      ["高级", "settings-advanced"],
      ["关于", "settings-about"],
    ];
    for (const [label, id] of ids) {
      expect(within(nav).getByRole("link", { name: label })).toHaveAttribute(
        "href",
        `#${id}`,
      );
      expect(document.getElementById(id)).toBeInTheDocument();
    }

    expect(screen.getByLabelText("Cookie secure")).not.toBeVisible();
    expect(screen.getByRole("button", { name: "安全" })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    toggleSection("安全");
    expect(screen.getByRole("button", { name: "安全" })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    expect(screen.getByLabelText("Cookie secure")).toBeVisible();

    fireEvent.click(bulkButton("收起全部"));
    expect(screen.getByLabelText("Cookie secure")).not.toBeVisible();
    expect(screen.getByLabelText("Poll interval")).not.toBeVisible();
    fireEvent.click(bulkButton("展开全部"));
    expect(screen.getByLabelText("Cookie secure")).toBeVisible();
    expect(screen.getByLabelText("Poll interval")).toBeVisible();
  });

  it("flags the dirty group in the nav and header until it is discarded", async () => {
    signedIn();
    renderApp("/settings");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    expect(screen.getByTestId("settings-nav-dirty-runner")).toBeInTheDocument();
    expect(
      within(screen.getByTestId("settings-header-actions")).getByText("1 项未保存"),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("settings-nav-dirty-model")).not.toBeInTheDocument();

    fireEvent.click(
      within(screen.getByTestId("settings-header-actions")).getByRole("button", {
        name: "放弃更改",
      }),
    );
    expect(screen.queryByTestId("settings-nav-dirty-runner")).not.toBeInTheDocument();
  });
});
