import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Sparkline } from "./Sparkline";

describe("Sparkline", () => {
  it("renders one bar per datum with an accessible total/peak summary", () => {
    const { container } = render(
      <Sparkline
        data={[
          { label: "10-01", value: 2 },
          { label: "10-02", value: 0 },
          { label: "10-03", value: 5 },
        ]}
        label="上报"
        valueFormatter={(value) => `${value} 次`}
      />,
    );

    const chart = screen.getByRole("img", {
      name: "上报，最近 3 天共 7 次，峰值 5 次（10-03）",
    });
    expect(chart.querySelectorAll("rect")).toHaveLength(3);
    expect(chart).toHaveAttribute("tabindex", "0");
    expect(container.querySelector("[style]")).toBeNull();
  });

  it("renders the empty label and no SVG when there are no buckets", () => {
    const { container } = render(<Sparkline data={[]} label="上报" emptyLabel="没有记录" />);

    expect(screen.getByText("没有记录")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(container.querySelector("svg")).toBeNull();
  });
});
