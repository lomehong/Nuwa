import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Button } from "./Button";

describe("Button", () => {
  it("renders variants and honours disabled", () => {
    render(
      <>
        <Button variant="secondary">次要</Button>
        <Button disabled>禁用</Button>
      </>,
    );

    expect(screen.getByRole("button", { name: "次要" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "禁用" })).toBeDisabled();
  });

  it("disables the button and marks it busy while pending", () => {
    render(<Button pending>保存</Button>);

    const button = screen.getByRole("button", { name: /保存/ });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("aria-busy", "true");
  });

  it("keeps a 44px touch floor and only compacts small buttons on fine pointers", () => {
    render(
      <>
        <Button size="sm">小号</Button>
        <Button size="md">中号</Button>
      </>,
    );

    const small = screen.getByRole("button", { name: "小号" });
    expect(small).toHaveClass("min-h-11");
    expect(small).toHaveClass("min-w-11");
    expect(small).toHaveClass("pointer-fine:min-h-9");
    expect(small).toHaveClass("pointer-fine:min-w-9");

    const medium = screen.getByRole("button", { name: "中号" });
    expect(medium).toHaveClass("min-h-11");
    expect(medium).not.toHaveClass("pointer-fine:min-h-9");
  });
});
