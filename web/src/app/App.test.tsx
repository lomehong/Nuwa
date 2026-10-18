import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import App from "./App";

describe("App", () => {
  it("renders the placeholder shell without inline styles", () => {
    const { container } = render(<App />);

    expect(screen.getByRole("heading", { level: 1, name: "Nuwa 控制台" })).toBeInTheDocument();
    expect(container.querySelector("[style]")).toBeNull();
  });
});
