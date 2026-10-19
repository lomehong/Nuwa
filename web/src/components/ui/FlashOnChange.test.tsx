import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { FlashOnChange } from "./FlashOnChange";

describe("FlashOnChange", () => {
  it("renders the current value", () => {
    const { rerender } = render(<FlashOnChange value={6} />);
    expect(screen.getByText("6")).toBeInTheDocument();
    rerender(<FlashOnChange value={9} />);
    expect(screen.getByText("9")).toBeInTheDocument();
    expect(screen.queryByText("6")).not.toBeInTheDocument();
  });

  it("only carries the flash class after the first change", () => {
    const { container, rerender } = render(<FlashOnChange value={1} />);
    const initial = container.firstElementChild as HTMLElement;
    expect(initial.classList.contains("nuwa-flash")).toBe(false);

    rerender(<FlashOnChange value={2} />);
    const flashed = container.firstElementChild as HTMLElement;
    expect(flashed.textContent).toBe("2");
    expect(flashed.classList.contains("nuwa-flash")).toBe(true);
  });
});
