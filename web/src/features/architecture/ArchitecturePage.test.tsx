import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";

import { ArchitecturePage } from "./ArchitecturePage";

describe("ArchitecturePage", () => {
  it("frames the embedded architecture artifact", () => {
    render(
      <MemoryRouter>
        <ArchitecturePage />
      </MemoryRouter>,
    );

    const section = screen.getByRole("region", { name: "架构图" });
    const frame = section.querySelector("iframe");
    expect(frame).not.toBeNull();
    expect(frame).toHaveAttribute("src", "/architecture.html");
    expect(frame).toHaveAttribute("title", "Nuwa 架构图");
  });
});
