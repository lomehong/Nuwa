import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import {
  makeMemory,
  makeMemoryList,
  mockMe,
  mockOverview,
  mockProjectMemory,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

const REPO = "example.com/acme/app";
const ROUTE = `/projects/${encodeURIComponent(REPO)}/memory`;

function fixture(disabled = false) {
  return makeMemoryList({
    data: [
      makeMemory({ disabled }),
      makeMemory({
        fingerprint: "fp-other",
        title: "TypeError in handler",
        state: "pr_open",
        commit_sha: "fedcba9876543210",
        pr_url: null,
        root_cause: "ROOT CAUSE: missing null guard",
        change_stat: "handler.ts | 4 ++--",
        files: 2,
      }),
    ],
    total: 2,
  });
}

function memoryCard(fingerprint: string) {
  const item = screen.getByRole("link", { name: fingerprint }).closest("li");
  if (!item) throw new Error("Missing memory card");
  return within(item);
}

describe("项目修复记忆", () => {
  it("lists provenance and disables a memory through the API", async () => {
    let patched: string | null = null;
    server.use(
      mockMe(),
      mockOverview(),
      mockProjectMemory(fixture()),
      http.patch(/\/api\/v1\/projects\/.*\/memory\/.*$/, async ({ request }) => {
        patched = await request.text();
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderApp(ROUTE);

    expect(await screen.findByRole("heading", { level: 2, name: "修复记忆" })).toBeInTheDocument();
    expect(await screen.findByText("panic in Average")).toBeInTheDocument();
    expect(screen.getByText(/ROOT CAUSE: loop bound/)).toBeInTheDocument();
    expect(screen.getByText("commit 0123456789ab")).toBeInTheDocument();
    expect(screen.getAllByRole("link", { name: /打开 PR/ }).length).toBeGreaterThan(0);
    expect(memoryCard("fp-memory").getByText("验证来源：local_verification")).toBeInTheDocument();
    expect(memoryCard("fp-memory").getByText("证据有效")).toBeInTheDocument();
    expect(memoryCard("fp-memory").getByText("可用于后续提示")).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "停用" })[0]);
    await waitFor(() => expect(patched).toContain('"disabled":true'));
  });

  it("requires confirmation before deleting and reports disabled state", async () => {
    let deleted = false;
    server.use(
      mockMe(),
      mockOverview(),
      mockProjectMemory(fixture(true)),
      http.delete(/\/api\/v1\/projects\/.*\/memory\/.*$/, () => {
        deleted = true;
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderApp(ROUTE);

    await screen.findByText("panic in Average");
    expect(screen.getAllByText("已停用").length).toBeGreaterThan(0);
    expect(screen.getAllByRole("button", { name: "启用" }).length).toBeGreaterThan(0);
    expect(memoryCard("fp-memory").getByText("证据有效")).toBeInTheDocument();
    expect(memoryCard("fp-memory").getByText("不用于后续提示")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "删除记忆：fp-memory" }));
    const dialog = screen.getByRole("dialog", { name: "删除这条修复记忆？" });
    fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));
    await waitFor(() => expect(deleted).toBe(true));
  });

  it("counts and filters prompt eligibility separately from manual disabling", async () => {
    server.use(
      mockMe(),
      mockOverview(),
      mockProjectMemory(
        makeMemoryList({
          data: [
            makeMemory({ fingerprint: "fp-valid", verification_source: "ci_green" }),
            makeMemory({ fingerprint: "fp-disabled", disabled: true }),
            makeMemory({
              fingerprint: "fp-legacy",
              evidence_valid: false,
              verification_source: "legacy_unknown",
            }),
            makeMemory({
              fingerprint: "fp-revoked",
              evidence_valid: false,
              revoked_at_ms: 1_700_000_001_000,
              revocation_reason: "CI failed",
            }),
            makeMemory({ fingerprint: "fp-invalid", evidence_valid: false }),
          ],
          total: 5,
        }),
      ),
    );
    renderApp(ROUTE);

    await screen.findByRole("link", { name: "fp-valid" });
    expect(screen.getByText("1 / 1")).toBeInTheDocument();
    expect(screen.getByText(/证据无效 3 条/)).toBeInTheDocument();
    expect(memoryCard("fp-valid").getByText("可用于后续提示")).toBeInTheDocument();
    for (const fingerprint of ["fp-disabled", "fp-legacy", "fp-revoked", "fp-invalid"]) {
      expect(memoryCard(fingerprint).getByText("不用于后续提示")).toBeInTheDocument();
    }
    expect(memoryCard("fp-revoked").getByText("撤销原因：CI failed")).toBeInTheDocument();
    expect(memoryCard("fp-revoked").getByText(/撤销于/)).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "active" } });
    const list = screen.getByRole("list", { name: "修复记忆列表" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    expect(within(list).getByRole("link", { name: "fp-valid" })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "invalid" } });
    expect(within(list).getAllByRole("listitem")).toHaveLength(3);
    expect(within(list).queryByRole("link", { name: "fp-valid" })).not.toBeInTheDocument();
    expect(within(list).queryByRole("link", { name: "fp-disabled" })).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "disabled" } });
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    expect(within(list).getByRole("link", { name: "fp-disabled" })).toBeInTheDocument();
  });

  it.each([
    {
      name: "legacy",
      evidenceLabel: "证据未验证",
      overrides: { evidence_valid: false, verification_source: "legacy_unknown" },
    },
    {
      name: "invalid",
      evidenceLabel: "证据无效",
      overrides: { evidence_valid: false, verification_source: "local_verification" },
    },
    {
      name: "revoked",
      evidenceLabel: "证据已撤销",
      overrides: {
        evidence_valid: false,
        verification_source: "ci_green",
        revoked_at_ms: 1_700_000_001_000,
        revocation_reason: "PR closed without merge",
      },
    },
  ])("enabling $name memory does not revive its evidence", async ({ evidenceLabel, overrides }) => {
    let entry = makeMemory({ ...overrides, disabled: true });
    let patched: unknown;
    server.use(
      mockMe(),
      mockOverview(),
      http.get(/\/api\/v1\/projects\/.*\/memory(\?.*)?$/, () =>
        HttpResponse.json(makeMemoryList({ data: [entry] })),
      ),
      http.patch(/\/api\/v1\/projects\/.*\/memory\/.*$/, async ({ request }) => {
        patched = await request.json();
        entry = { ...entry, disabled: false };
        return new HttpResponse(null, { status: 204 });
      }),
    );
    renderApp(ROUTE);

    await screen.findByRole("link", { name: entry.fingerprint });
    expect(memoryCard(entry.fingerprint).getByText(evidenceLabel)).toBeInTheDocument();
    expect(memoryCard(entry.fingerprint).getByText("不用于后续提示")).toBeInTheDocument();
    expect(screen.getByText(/重新启用只清除手动停用，不会恢复已撤销证据/)).toBeInTheDocument();
    fireEvent.click(memoryCard(entry.fingerprint).getByRole("button", { name: "启用" }));

    await waitFor(() => expect(patched).toEqual({ disabled: false }));
    await waitFor(() => {
      expect(memoryCard(entry.fingerprint).getByText("未停用")).toBeInTheDocument();
    });
    expect(memoryCard(entry.fingerprint).getByText(evidenceLabel)).toBeInTheDocument();
    expect(memoryCard(entry.fingerprint).getByText("不用于后续提示")).toBeInTheDocument();
    expect(memoryCard(entry.fingerprint).queryByText("可用于后续提示")).not.toBeInTheDocument();
    expect(memoryCard(entry.fingerprint).getByText(`验证来源：${entry.verification_source}`))
      .toBeInTheDocument();
    if (entry.revocation_reason) {
      expect(memoryCard(entry.fingerprint).getByText(`撤销原因：${entry.revocation_reason}`))
        .toBeInTheDocument();
    }
    expect(screen.getByText("0 / 0")).toBeInTheDocument();
    expect(await screen.findByText("仅清除手动停用；无效或已撤销的证据仍不会用于提示。"))
      .toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "active" } });
    expect(screen.getByText("没有匹配的记忆")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: entry.fingerprint })).not.toBeInTheDocument();
  });
});
