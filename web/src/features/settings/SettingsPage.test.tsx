import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  makeIssue,
  makeProcessConfiguration,
  makeSavedConfig,
  makeSettings,
  makeStartupConfig,
  mockIssues,
  mockMe,
  mockOverviewResources,
  mockSettings,
  OLD_REVISION,
  SAVED_REVISION,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";
import { SETTINGS_POLL_MS, settingsQueryKey } from "./queries";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
});

afterEach(() => vi.useRealTimers());

function signedIn(settings: Parameters<typeof mockSettings>[0] = {}) {
  server.use(mockMe(), ...mockOverviewResources(), mockSettings(settings));
}

function saveButton() {
  return screen.getByTestId("settings-save");
}

function settingsNav() {
  return screen.getByRole("group", { name: "设置区域" });
}

function areaLink(name: string | RegExp) {
  return within(settingsNav()).getByRole("link", { name });
}

function shellNavLink(name: string) {
  const nav = screen.getByRole("navigation", { name: "主导航" });
  return within(nav).getByRole("link", { name });
}

const LEAVE_DIALOG = "放弃未保存的更改？";

async function expectLeaveDialog() {
  return screen.findByRole("dialog", { name: LEAVE_DIALOG });
}

describe("SettingsPage", () => {
  it("redirects /settings to connection and switches one area per view", async () => {
    signedIn();
    renderApp("/settings");

    expect(await screen.findByRole("heading", { level: 1, name: "连接" })).toBeInTheDocument();
    await screen.findByText("GitHub token");
    expect(areaLink("连接")).toHaveAttribute("aria-current", "page");
    expect(areaLink("执行")).toHaveAttribute("href", "/settings/execution");
    expect(screen.getByText("GitHub token")).toBeInTheDocument();
    expect(screen.getByLabelText("LLM Provider")).toBeInTheDocument();

    fireEvent.click(areaLink("执行"));
    expect(await screen.findByRole("heading", { level: 1, name: "执行" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toBeInTheDocument();
    expect(screen.queryByText("GitHub token")).not.toBeInTheDocument();
    expect(areaLink("执行")).toHaveAttribute("aria-current", "page");

    fireEvent.click(areaLink("治理"));
    expect(await screen.findByRole("heading", { level: 1, name: "治理" })).toBeInTheDocument();
    expect(screen.getByLabelText("Cookie secure")).toBeInTheDocument();
    expect(screen.getByLabelText("Stuck floor")).toBeInTheDocument();

    fireEvent.click(areaLink("环境"));
    expect(await screen.findByRole("heading", { level: 1, name: "环境" })).toBeInTheDocument();
    expect(screen.getByLabelText("Serve unit")).toBeInTheDocument();
    expect(screen.getByText("Env file")).toBeInTheDocument();
  });

  it("saves only this area's keys and reads saved revision and activation from the server", async () => {
    let body: unknown;
    let current = makeSettings();
    signedIn();
    server.use(
      http.get("/api/v1/settings", () => HttpResponse.json(current)),
      http.put("/api/v1/settings", async ({ request }) => {
        body = await request.json();
        current = makeSettings({
          saved: makeSavedConfig({ revision: OLD_REVISION }),
          values: { ...current.values, NUWA_INTERVAL: { value: "60", source: "env" } },
          processes: [makeProcessConfiguration({ activation: "stale" })],
        });
        return HttpResponse.json({ changed: ["NUWA_INTERVAL"], restart_required: true });
      }),
    );
    renderApp("/settings/execution");

    const interval = await screen.findByLabelText("Poll interval");
    fireEvent.change(interval, { target: { value: "60" } });
    expect(screen.getByText(/1 项未保存/)).toBeInTheDocument();
    fireEvent.click(saveButton());

    await waitFor(() => expect(body).toEqual({ values: { NUWA_INTERVAL: 60 } }));
    expect(await screen.findByText("设置已保存")).toBeInTheDocument();
    const saved = await screen.findByRole("region", { name: "保存状态" });
    await waitFor(() => expect(within(saved).getByText(OLD_REVISION)).toBeInTheDocument());
    expect(screen.getByText(/存在旧版本进程/)).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);
    const work = screen.getByRole("region", { name: "work / runner-1 配置" });
    fireEvent.click(within(work).getByText("启动时实际配置与来源"));
    expect(within(work).getByText("30 · 来源：文件（file）")).toBeInTheDocument();
    await waitFor(() =>
      expect(within(settingsNav()).queryByText("有未保存更改")).not.toBeInTheDocument(),
    );
    expect(screen.getByText(/没有未保存的更改/)).toBeInTheDocument();
  });

  it("never renders a stored secret value", async () => {
    signedIn();
    renderApp("/settings/connection");

    expect((await screen.findAllByText("已设置 · 末 4 位 1234")).length).toBeGreaterThan(0);
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
    signedIn();
    renderApp("/settings/execution");

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
      http.put("/api/v1/settings", () => {
        put = true;
        return HttpResponse.json({ changed: [], restart_required: false });
      }),
    );
    signedIn();
    renderApp("/settings/execution");

    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "0" },
    });
    fireEvent.click(saveButton());

    expect(await screen.findByText("必须在 1–86400 之间")).toBeInTheDocument();
    expect(put).toBe(false);
  });

  it("tracks dirty drafts in the nav and discards them", async () => {
    signedIn();
    renderApp("/settings/execution");
    const interval = await screen.findByLabelText("Poll interval");

    fireEvent.change(interval, { target: { value: "60" } });
    expect(screen.getByText(/1 项未保存/)).toBeInTheDocument();
    expect(within(settingsNav()).getByText("有未保存更改")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "放弃" }));
    expect(screen.queryByText(/1 项未保存/)).not.toBeInTheDocument();
    expect(screen.getByText(/没有未保存的更改/)).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(30);
    expect(within(settingsNav()).queryByText("有未保存更改")).not.toBeInTheDocument();
  });

  it("guards navigation to another settings area while drafts are unsaved", async () => {
    signedIn();
    renderApp("/settings/execution");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    fireEvent.click(areaLink("治理"));
    const dialog = await expectLeaveDialog();
    fireEvent.click(within(dialog).getByRole("button", { name: "留在本页" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("heading", { level: 1, name: "执行" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);

    fireEvent.click(areaLink("治理"));
    fireEvent.click(await screen.findByRole("button", { name: "放弃并离开" }));
    expect(await screen.findByRole("heading", { level: 1, name: "治理" })).toBeInTheDocument();
    expect(screen.queryByText(/1 项未保存/)).not.toBeInTheDocument();
    expect(areaLink("治理")).toHaveAttribute("aria-current", "page");
  });

  it("guards in-app navigation while drafts are unsaved", async () => {
    signedIn();
    renderApp("/settings/execution");
    const interval = await screen.findByLabelText("Poll interval");
    fireEvent.change(interval, { target: { value: "60" } });

    fireEvent.click(shellNavLink("总览"));
    const dialog = await expectLeaveDialog();
    expect(within(dialog).getByRole("button", { name: "留在本页" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "放弃并离开" })).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "留在本页" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("heading", { level: 1, name: "执行" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);

    fireEvent.click(shellNavLink("总览"));
    fireEvent.click(await screen.findByRole("button", { name: "放弃并离开" }));
    expect(await screen.findByRole("heading", { level: 1, name: "总览" })).toBeInTheDocument();
  });

  it("ignores same-route navigation that differs only by a trailing slash", async () => {
    signedIn();
    const { router } = renderApp("/settings/execution");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    await act(async () => {
      await router.navigate("/settings/execution/");
    });

    expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument();
    expect(screen.getByText(/1 项未保存/)).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);
  });

  it("ignores same-route navigation whose query parameters are reordered", async () => {
    signedIn();
    const { router } = renderApp("/settings/execution?a=1&b=2");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    await act(async () => {
      await router.navigate("/settings/execution?b=2&a=1");
    });

    expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument();
    expect(screen.getByText(/1 项未保存/)).toBeInTheDocument();
  });

  it("guards the command palette's programmatic navigation while dirty", async () => {
    server.use(
      mockIssues({
        data: [makeIssue({ fingerprint: "fp-palette", title: "Palette target panic" })],
      }),
    );
    signedIn();
    renderApp("/settings/execution");
    fireEvent.change(await screen.findByLabelText("Poll interval"), {
      target: { value: "60" },
    });

    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    const palette = await screen.findByRole("dialog", { name: "搜索问题单" });
    const input = within(palette).getByRole("searchbox", { name: "搜索问题单" });
    fireEvent.change(input, { target: { value: "panic" } });
    await within(palette).findByRole("link", { name: /Palette target panic/ });
    fireEvent.keyDown(input, { key: "Enter" });

    const dialog = await screen.findByRole("dialog", { name: LEAVE_DIALOG });
    fireEvent.click(within(dialog).getByRole("button", { name: "留在本页" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: LEAVE_DIALOG })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("heading", { level: 1, name: "执行" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);
  });

  it("guards browser back (POP) while drafts are unsaved", async () => {
    signedIn();
    server.use(mockIssues());
    const { router } = renderApp("/");
    await act(async () => {
      await router.navigate("/settings/execution");
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
    expect(screen.getByRole("heading", { level: 1, name: "执行" })).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(60);

    await act(async () => {
      await router.navigate(-1);
    });
    fireEvent.click(await screen.findByRole("button", { name: "放弃并离开" }));
    expect(await screen.findByRole("heading", { level: 1, name: "总览" })).toBeInTheDocument();
  });

  it("closes the drawer behind the guard and unlocks scroll after cancel", async () => {
    signedIn();
    renderApp("/settings/execution");
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

  it("explains admission and delivery checks without claiming hard runtime token caps", async () => {
    signedIn();
    renderApp("/settings/execution");

    expect(await screen.findByLabelText("Daily PR admission slots (UTC)")).toHaveValue(5);
    expect(screen.getByLabelText("Per-repair token delivery gate")).toBeInTheDocument();
    expect(screen.getByLabelText("Daily token admission / delivery budget")).toBeInTheDocument();
    expect(screen.getByLabelText("Rolling 30-day token admission / delivery budget")).toBeInTheDocument();
    expect(screen.getByText("槽位")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "准入预算与交付门" })).toBeInTheDocument();
    const explanation = screen.getByText(/PR 按 UTC 准入日槽位计费/);
    for (const text of [
      "重启 work 后重新上报确认，CLI 覆盖仍优先",
      "包含在途与结果不明，重试保留原日",
      "单次 token 预算是修复交付门",
      "准入及交付前检查已记录用量，失败也计入",
      "并发或在途消耗可能超额",
      "并非运行时或 provider 硬上限",
      "策略由创建进程解析，各进程可能不同",
    ]) {
      expect(explanation).toHaveTextContent(text);
    }
    expect(screen.queryByLabelText("Monthly token limit")).not.toBeInTheDocument();
  });

  it("explicitly probes saved GitHub candidates and draft LLM candidates, never live inputs", async () => {
    const bodies: Array<Record<string, unknown>> = [];
    const csrf: Array<string | null> = [];
    const base = makeSettings();
    const providerSpec = base.schema.find((spec) => spec.key === "NUWA_PROVIDER")!;
    signedIn({
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
    });
    server.use(
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
    renderApp("/settings/connection");

    fireEvent.click(await screen.findByRole("button", { name: "测试 GitHub token（已保存候选）" }));
    await waitFor(() => expect(bodies[0]).toEqual({ target: "github", candidate: "saved" }));
    expect(await screen.findByText("token accepted; 1/2")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("LLM Provider"), { target: { value: "zai" } });
    fireEvent.change(screen.getByLabelText("Model"), {
      target: { value: "glm-5.3-flash" },
    });
    fireEvent.click(screen.getByRole("button", { name: "测试模型配置（草稿候选）" }));

    await waitFor(() =>
      expect(bodies[1]).toEqual({
        target: "llm",
        candidate: "draft",
        values: { NUWA_PROVIDER: "zai", NUWA_MODEL: "glm-5.3-flash" },
      }),
    );
    expect(await screen.findByText("zai model responded")).toBeInTheDocument();
    expect(csrf).toEqual(["csrf-token", "csrf-token"]);
  });

  it("probes GitHub with the typed token instead of the stored one", async () => {
    const bodies: Array<Record<string, unknown>> = [];
    signedIn();
    server.use(
      http.post("/api/v1/settings/test", async ({ request }) => {
        bodies.push((await request.json()) as Record<string, unknown>);
        return HttpResponse.json({ ok: true, detail: "token accepted; 2/3" });
      }),
    );
    renderApp("/settings/connection");

    fireEvent.click(await screen.findByRole("button", { name: "更新 GitHub token" }));
    fireEvent.change(screen.getByPlaceholderText("留空保持不变"), {
      target: { value: "ghp_candidate_token" },
    });
    fireEvent.click(screen.getByRole("button", { name: "测试 GitHub token（草稿候选）" }));

    await waitFor(() =>
      expect(bodies[0]).toEqual({
        target: "github",
        candidate: "draft",
        values: { NUWA_GITHUB_TOKEN: "ghp_candidate_token" },
      }),
    );
    expect(JSON.stringify(bodies[0])).not.toContain("1234");
  });

  it("keeps an explicitly cleared LLM value in the draft candidate", async () => {
    const base = makeSettings();
    const provider = base.schema.find((spec) => spec.key === "NUWA_PROVIDER")!;
    signedIn({
      schema: [...base.schema, { ...provider, key: "NUWA_MODEL", label: "Model", type: "string", options: [] }],
      values: { ...base.values, NUWA_MODEL: { value: "saved-model", source: "env" } },
    });
    let body: unknown;
    server.use(http.post("/api/v1/settings/test", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ok: true, detail: "candidate accepted" });
    }));
    renderApp("/settings/connection");
    fireEvent.change(await screen.findByLabelText("Model"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "测试模型配置（草稿候选）" }));
    await waitFor(() => expect(body).toEqual({
      target: "llm", candidate: "draft", values: { NUWA_MODEL: "" },
    }));
  });

  it("surfaces a probe 422 as a danger toast and an inline field message", async () => {
    signedIn();
    server.use(
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
    renderApp("/settings/connection");

    fireEvent.click(await screen.findByRole("button", { name: "测试 GitHub token（已保存候选）" }));

    expect(await screen.findByText("GitHub token 测试失败")).toBeInTheDocument();
    expect(screen.getByText("NUWA_GITHUB_TOKEN：token is required")).toBeInTheDocument();
    expect(screen.getByText("token is required")).toBeInTheDocument();
  });

  it("clears a probe field error once a later probe succeeds", async () => {
    let calls = 0;
    signedIn();
    server.use(
      http.post("/api/v1/settings/test", () => {
        calls += 1;
        if (calls === 1) {
          return HttpResponse.json(
            {
              error: {
                code: "validation_failed",
                message: "invalid settings",
                fields: { NUWA_PROVIDER: "must be anthropic" },
              },
            },
            { status: 422 },
          );
        }
        return HttpResponse.json({ ok: true, detail: "model responded" });
      }),
    );
    renderApp("/settings/connection");

    const probe = await screen.findByRole("button", { name: "测试模型配置（已保存候选）" });
    fireEvent.click(probe);
    expect(await screen.findByText("must be anthropic")).toBeInTheDocument();
    expect(screen.getByLabelText("LLM Provider")).toHaveAttribute("aria-invalid", "true");

    fireEvent.click(probe);
    expect(await screen.findByText("model responded")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("must be anthropic")).not.toBeInTheDocument());
    expect(screen.getByLabelText("LLM Provider")).not.toHaveAttribute("aria-invalid");
  });

  it("keeps drafts edited while the save request is pending", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    signedIn();
    server.use(
      http.put("/api/v1/settings", async () => {
        await gate;
        return HttpResponse.json({ changed: ["NUWA_INTERVAL"], restart_required: false });
      }),
    );
    renderApp("/settings/execution");
    const interval = await screen.findByLabelText("Poll interval");
    fireEvent.change(interval, { target: { value: "60" } });
    fireEvent.click(saveButton());
    fireEvent.change(interval, { target: { value: "90" } });
    release();

    expect(await screen.findByText("设置已保存")).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(90);
    expect(screen.getByText(/1 项未保存/)).toBeInTheDocument();
  });

  it("keeps migration notices but never uses legacy merged fields as activation authority", async () => {
    signedIn({ restart_pending: true, needs_migration: true });
    renderApp("/settings/execution");

    expect(await screen.findByText(/检测到旧格式键名/)).toBeInTheDocument();
    expect(screen.queryByText(/文件值 30 · 运行中 60/)).not.toBeInTheDocument();
    expect(screen.queryByText(/有配置等待重启|存在旧版本进程/)).not.toBeInTheDocument();
    expect(screen.getByText("配置状态：已加载保存版本（active）")).toBeInTheDocument();
  });

  it("warns when the console runs in local mode", async () => {
    server.use(
      mockMe({ local_mode: true, expires_at_ms: null }),
      ...mockOverviewResources(),
      mockSettings(),
    );
    renderApp("/settings/connection");

    expect(
      await screen.findByText(/任何能访问该端口的客户端都可修改设置/),
    ).toBeInTheDocument();
  });

  it("keeps each runner's revision and freshness independent and scopes rows to this area", async () => {
    signedIn({
      processes: [
        makeProcessConfiguration({ role: "serve" }),
        makeProcessConfiguration({
          snapshot: makeStartupConfig({ runner_id: "runner-current" }),
          freshness: "offline",
        }),
        makeProcessConfiguration({
          snapshot: makeStartupConfig({
            runner_id: "runner-old",
            process_instance_id: "66666666-6666-4666-8666-666666666666",
            loaded_revision: OLD_REVISION,
            values: { NUWA_INTERVAL: { value: "75", source: "environment", reason: null } },
          }),
          activation: "stale",
        }),
        makeProcessConfiguration({
          runner_id: "runner-legacy",
          snapshot: null,
          activation: "unknown",
          freshness: "unreported",
          last_seen_at_ms: null,
          reason: "startup_snapshot_unavailable",
        }),
      ],
    });
    renderApp("/settings/execution");
    const current = await screen.findByRole("region", { name: "work / runner-current 配置" });
    expect(within(current).getByText("配置状态：已加载保存版本（active）")).toBeInTheDocument();
    expect(within(current).getByText("上报状态：离线（offline）")).toBeInTheDocument();
    expect(within(current).getByText(SAVED_REVISION)).toBeInTheDocument();
    expect(within(current).getByText(/仅有历史启动证据/)).toBeInTheDocument();

    const old = screen.getByRole("region", { name: "work / runner-old 配置" });
    expect(within(old).getByText("配置状态：旧版本（stale）")).toBeInTheDocument();
    expect(within(old).getByText("上报状态：在线（online）")).toBeInTheDocument();
    expect(within(old).getByText(OLD_REVISION)).toBeInTheDocument();
    fireEvent.click(within(old).getByText("启动时实际配置与来源"));
    expect(within(old).getByText("75 · 来源：环境变量（environment）")).toBeInTheDocument();
    const legacy = screen.getByRole("region", { name: "work / runner-legacy 配置" });
    expect(within(legacy).getByText(/未知（unknown），不能确认已生效/)).toBeInTheDocument();
    expect(within(legacy).getByText("上报状态：未上报（unreported）")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "serve 配置" })).not.toBeInTheDocument();
    expect(screen.queryByText("下一次轮询")).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "候选配置预览" })).toHaveTextContent("已保存候选");
  });

  it("does not imply work activation after only serve has restarted", async () => {
    signedIn({
      restart_pending: false,
      effective_diff: {},
      processes: [
        makeProcessConfiguration({ role: "serve" }),
        makeProcessConfiguration({
          activation: "stale",
          snapshot: makeStartupConfig({ loaded_revision: OLD_REVISION }),
        }),
      ],
    });
    renderApp("/settings/connection");
    const serve = await screen.findByRole("region", { name: "serve 配置" });
    expect(within(serve).getByText("配置状态：已加载保存版本（active）")).toBeInTheDocument();
    const work = screen.getByRole("region", { name: "work / runner-1 配置" });
    expect(within(work).getByText("配置状态：旧版本（stale）")).toBeInTheDocument();
    expect(screen.getByText(/存在旧版本进程/)).toBeInTheDocument();
  });

  it("fails closed on unknown saved/startup evidence and explains persistent CLI overrides", async () => {
    signedIn({
      saved: makeSavedConfig({ state: "unknown", revision: null, reason: "file_db_mismatch" }),
      processes: [makeProcessConfiguration({
        activation: "unknown",
        reason: "startup_revision_unproven",
        snapshot: makeStartupConfig({
          loaded_revision: null,
          revision_reason: "effective_override",
          values: { NUWA_INTERVAL: { value: "17", source: "cli", reason: null } },
        }),
      })],
    });
    renderApp("/settings/execution");
    expect(await screen.findByText("保存状态：未知（unknown），不能确认保存版本")).toBeInTheDocument();
    const work = screen.getByRole("region", { name: "work / runner-1 配置" });
    expect(within(work).getByText("配置状态：未知（unknown），不能确认已生效")).toBeInTheDocument();
    expect(within(work).getByText("上报状态：在线（online）")).toBeInTheDocument();
    expect(within(work).getByText(/使用相同命令重启仍保留 CLI 覆盖/)).toBeInTheDocument();
    fireEvent.click(within(work).getByText("启动时实际配置与来源"));
    expect(within(work).getByText("17 · 来源：命令行（cli）")).toBeInTheDocument();
    expect(screen.queryByText(/配置状态：已加载保存版本/)).not.toBeInTheDocument();
  });

  it("polls process activation, clears stale hints despite legacy flags and preserves local drafts", async () => {
    // Fake only intervals: real async route/MSW readiness remains observable.
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    let current = makeSettings({
      restart_pending: true,
      processes: [makeProcessConfiguration({
        activation: "stale",
        snapshot: makeStartupConfig({ loaded_revision: OLD_REVISION }),
      })],
    });
    signedIn();
    server.use(http.get("/api/v1/settings", () => HttpResponse.json(current)));
    renderApp("/settings/execution");
    const interval = await screen.findByLabelText("Poll interval");
    expect(await screen.findByText(/存在旧版本进程/)).toBeInTheDocument();
    fireEvent.change(interval, { target: { value: "90" } });
    current = makeSettings({ restart_pending: true });

    await act(async () => { await vi.advanceTimersByTimeAsync(SETTINGS_POLL_MS); });
    await waitFor(() => expect(screen.queryByText(/存在旧版本进程/)).not.toBeInTheDocument());
    expect(screen.getByLabelText("Poll interval")).toHaveValue(90);
    expect(screen.getByText(/1 项未保存/)).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "候选配置预览" })).toHaveTextContent("草稿候选");
    expect(screen.getByText("配置状态：已加载保存版本（active）")).toBeInTheDocument();
  });

  it("removes stale hints after activation refresh even after a restart-required save", async () => {
    let current = makeSettings();
    signedIn();
    server.use(
      http.get("/api/v1/settings", () => HttpResponse.json(current)),
      http.put("/api/v1/settings", () => {
        current = makeSettings({ processes: [makeProcessConfiguration({ activation: "stale", snapshot: makeStartupConfig({ loaded_revision: OLD_REVISION }) })] });
        return HttpResponse.json({ changed: ["NUWA_INTERVAL"], restart_required: true });
      }),
    );
    const { queryClient } = renderApp("/settings/execution");
    fireEvent.change(await screen.findByLabelText("Poll interval"), { target: { value: "60" } });
    fireEvent.click(saveButton());
    expect(await screen.findByText(/存在旧版本进程/)).toBeInTheDocument();
    current = makeSettings({ restart_pending: true });
    await act(async () => { await queryClient.invalidateQueries({ queryKey: settingsQueryKey }); });
    await waitFor(() => expect(screen.queryByText(/存在旧版本进程/)).not.toBeInTheDocument());
    expect(screen.getByText("配置状态：已加载保存版本（active）")).toBeInTheDocument();
  });

  it("warns that a console token change ends the session on restart", async () => {
    const base = makeSettings();
    const secretSpec = base.schema.find((spec) => spec.secret)!;
    signedIn({
      schema: [
        ...base.schema,
        { ...secretSpec, key: "NUWA_UI_TOKEN", label: "Console token" },
      ],
      values: {
        ...base.values,
        NUWA_UI_TOKEN: { set: false, last4: null, updated_at_ms: 0 },
      },
    });
    server.use(
      http.put("/api/v1/settings", () =>
        HttpResponse.json({ changed: ["NUWA_UI_TOKEN"], restart_required: true }),
      ),
    );
    renderApp("/settings/connection");

    fireEvent.click(await screen.findByRole("button", { name: "设置 Console token" }));
    fireEvent.change(screen.getByPlaceholderText("留空保持不变"), {
      target: { value: "new-console-token" },
    });
    fireEvent.click(saveButton());

    expect(
      await screen.findByText(
        "更改控制台令牌后，重启会使当前会话失效，需要重新登录。",
      ),
    ).toBeInTheDocument();
  });

  it("renders the read-only env path and environment details", async () => {
    signedIn();
    renderApp("/settings/environment");

    expect(await screen.findByText("Env file")).toBeInTheDocument();
    expect(screen.queryByLabelText("Env file")).not.toBeInTheDocument();
    expect(screen.getAllByText("/opt/nuwa/nuwa.env").length).toBeGreaterThan(0);
    expect(screen.getByText("LEGACY_KEY")).toBeInTheDocument();
    expect(screen.getByText("NUWA_MAX_TURNS")).toBeInTheDocument();
  });
});
