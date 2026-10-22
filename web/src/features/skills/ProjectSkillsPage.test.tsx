import { fireEvent, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { makeProjectSkills, makeSkill, mockMe, mockOverview, mockProjectSkills, server } from "../../test/msw";
import { renderApp } from "../../test/render";

const REPO = "example.com/acme/app";
const ROUTE = `/projects/${encodeURIComponent(REPO)}/skills`;

function skillsFixture() {
  return makeProjectSkills({
    repo: REPO,
    selection_mode: "pinned",
    allowlist: ["rust-cargo", "missing"],
    warnings: ["仓库技能 `rust-cargo` 重复：b.md 覆盖 a.md", "skills 白名单里的 `missing` 不存在"],
    skills: [
      makeSkill({
        name: "rust-cargo",
        effective: false,
        origin: "builtin:rust-cargo.md",
      }),
      makeSkill({
        name: "rust-cargo",
        version: 7,
        description: "本仓 Rust 修复路径",
        source: "repo",
        origin: ".nuwa/skills/rust-cargo.md",
        effective: true,
        overrides_builtin: true,
        allowlisted: true,
        body: "LOCAL BODY",
      }),
      makeSkill({
        name: "node-toolchain",
        description: "Node 修复路径",
        languages: ["node"],
        matches: ["pnpm"],
        origin: "builtin:node-toolchain.md",
      }),
    ],
  });
}

describe("项目修复技能", () => {
  it("shows the catalog, pinned mode, warnings and effective overrides", async () => {
    server.use(mockMe(), mockOverview(), mockProjectSkills(skillsFixture()));
    renderApp(ROUTE);

    expect(await screen.findByRole("heading", { level: 2, name: "修复技能" })).toBeInTheDocument();
    expect(await screen.findByText("白名单钉住")).toBeInTheDocument();
    expect(await screen.findByText("2 个")).toBeInTheDocument();
    expect(screen.getAllByText("missing").length).toBeGreaterThan(0);
    expect(screen.getByText(/b\.md 覆盖 a\.md/)).toBeInTheDocument();
    expect(screen.getByText("node-toolchain")).toBeInTheDocument();

    // Default view hides the shadowed built-in definition.
    expect(screen.queryByText("已覆盖")).not.toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("只显示生效定义"));
    expect(await screen.findByText("已覆盖")).toBeInTheDocument();
    expect(screen.getByText("覆盖内置")).toBeInTheDocument();
    expect(screen.getByText("LOCAL BODY")).toBeInTheDocument();
  });

  it("filters by search and language", async () => {
    server.use(mockMe(), mockOverview(), mockProjectSkills(skillsFixture()));
    renderApp(ROUTE);

    await screen.findByText("node-toolchain");
    const search = screen.getByLabelText("搜索技能");
    fireEvent.change(search, { target: { value: "node" } });
    expect(screen.getByText("node-toolchain")).toBeInTheDocument();
    expect(screen.queryByText(/LOCAL BODY/)).not.toBeInTheDocument();

    fireEvent.change(search, { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("语言"), { target: { value: "rust" } });
    expect(screen.getByText(/LOCAL BODY/)).toBeInTheDocument();
    expect(screen.queryByText("node-toolchain")).not.toBeInTheDocument();
  });
});
