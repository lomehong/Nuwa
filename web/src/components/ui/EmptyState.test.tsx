import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { EmptyState } from "./EmptyState";

describe("EmptyState", () => {
  it("renders the title, description and action with a status role", () => {
    render(
      <EmptyState
        title="还没有问题单"
        description="接入 SDK 后会显示运行时信号。"
        action={<button type="button">查看接入手册</button>}
      />,
    );

    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "还没有问题单" })).toBeInTheDocument();
    expect(screen.getByText("接入 SDK 后会显示运行时信号。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "查看接入手册" })).toBeInTheDocument();
  });
});
