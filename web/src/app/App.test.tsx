import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { mockMe, mockOverview, server } from "../test/msw";
import App from "./App";

describe("App", () => {
  it("boots into the authenticated shell without inline styles", async () => {
    server.use(mockMe(), mockOverview());

    const { container } = render(<App />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "总览" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "主导航" })).toBeInTheDocument();
    expect(container.querySelector("[style]")).toBeNull();
  });
});
