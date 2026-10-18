import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Select } from "./Select";

describe("Select", () => {
  it("associates the label and renders the supplied options", () => {
    render(
      <Select
        label="严重度"
        options={[
          { value: "critical", label: "严重" },
          { value: "low", label: "低" },
        ]}
      />,
    );

    const select = screen.getByLabelText("严重度");
    expect(select).toHaveValue("critical");
    expect(screen.getByRole("option", { name: "低" })).toBeInTheDocument();
  });
});
