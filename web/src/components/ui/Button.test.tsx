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
});
