import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Input } from "./Input";

describe("Input", () => {
  it("associates the label with the input", () => {
    render(<Input label="仓库地址" placeholder="owner/repo" />);

    const input = screen.getByLabelText("仓库地址");
    expect(input).toHaveAttribute("placeholder", "owner/repo");
  });

  it("links hint and error text through aria-describedby and flags invalid", () => {
    render(<Input label="仓库" hint="例如 owner/repo" error="仓库不能为空" />);

    const input = screen.getByLabelText("仓库");
    const error = screen.getByRole("alert");

    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(error).toHaveTextContent("仓库不能为空");
    expect(screen.getByText("例如 owner/repo")).toBeInTheDocument();

    const describedBy = input.getAttribute("aria-describedby") ?? "";
    expect(describedBy).toContain(error.id);
    expect(describedBy.split(" ")).toHaveLength(2);
  });
});
