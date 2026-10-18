import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";

import {
  makeSettings,
  mockMe,
  mockOverviewResources,
  mockSettings,
  mockSystem,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

beforeEach(() => {
  document.cookie = "nuwa_csrf=csrf-token; path=/";
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
    fireEvent.click(screen.getByRole("button", { name: "保存更改" }));

    await waitFor(() => expect(body).toEqual({ values: { NUWA_INTERVAL: 60 } }));
    expect(await screen.findByText("重启后生效")).toBeInTheDocument();
    expect(screen.getByText("NUWA_INTERVAL")).toBeInTheDocument();
    expect(screen.getByText("设置已保存")).toBeInTheDocument();
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
    fireEvent.click(screen.getByRole("button", { name: "保存更改" }));

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
    fireEvent.click(screen.getByRole("button", { name: "保存更改" }));

    expect(await screen.findByText("必须在 1–86400 之间")).toBeInTheDocument();
    expect(put).toBe(false);
  });

  it("tracks dirty drafts and discards them", async () => {
    signedIn();
    renderApp("/settings");
    const interval = await screen.findByLabelText("Poll interval");

    fireEvent.change(interval, { target: { value: "60" } });
    expect(screen.getByText("1 项未保存")).toBeInTheDocument();
    expect(screen.getByText("未保存 1 项")).toBeInTheDocument();
    const saveBar = screen.getByTestId("settings-save-bar");
    expect(saveBar).toHaveClass("bottom-14", "z-20", "lg:static");
    expect(saveBar).not.toHaveClass("bottom-0");

    fireEvent.click(screen.getByRole("button", { name: "放弃更改" }));
    expect(screen.queryByText("1 项未保存")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(30);
  });

  it("renders every schema group plus service, advanced and about", async () => {
    signedIn();
    renderApp("/settings");

    await screen.findByRole("heading", { name: "凭据" });
    for (const title of ["模型", "Runner", "服务控制", "安全", "运维", "高级", "关于"]) {
      expect(screen.getByRole("heading", { name: title })).toBeInTheDocument();
    }
    expect(screen.getAllByText("0.2.0").length).toBeGreaterThan(0);
    expect(screen.getByText("MIT")).toBeInTheDocument();
    const repo = screen.getByRole("link", { name: /github\.com\/lomehong\/nuwa/ });
    expect(repo).toHaveAttribute("rel", "noreferrer");
    expect(screen.getByText("n/a")).toHaveClass("text-text-muted");
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
    fireEvent.click(screen.getByRole("button", { name: "保存更改" }));

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
    fireEvent.click(screen.getByRole("button", { name: "保存更改" }));
    // Retype while the PUT is still in flight: this draft was not submitted.
    fireEvent.change(interval, { target: { value: "90" } });
    release();

    expect(await screen.findByText("设置已保存")).toBeInTheDocument();
    expect(screen.getByLabelText("Poll interval")).toHaveValue(90);
    expect(screen.getByText("1 项未保存")).toBeInTheDocument();
  });
});
