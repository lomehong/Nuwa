import { render, screen, within } from "@testing-library/react";
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

  it("exposes every daily value as accessible text linked from the chart", () => {
    render(
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
    const table = screen.getByRole("table", { name: "上报 每日数值（3 天）" });

    expect(chart).toHaveAttribute("aria-describedby", table.id);
    expect(within(table).getByRole("columnheader", { name: "日期" })).toBeInTheDocument();
    expect(within(table).getByRole("columnheader", { name: "数值" })).toBeInTheDocument();
    expect(within(table).getByRole("rowheader", { name: "10-01" })).toBeInTheDocument();
    expect(within(table).getByText("2 次").closest("tr")).toHaveTextContent("10-01");
    expect(within(table).getByText("0 次").closest("tr")).toHaveTextContent("10-02");
    expect(within(table).getByText("5 次").closest("tr")).toHaveTextContent("10-03");
  });

  it("renders a flat baseline for an all-zero series", () => {
    const { container } = render(
      <Sparkline
        data={[
          { label: "10-01", value: 0 },
          { label: "10-02", value: 0 },
          { label: "10-03", value: 0 },
        ]}
        label="上报"
        valueFormatter={(value) => `${value} 次`}
      />,
    );

    const chart = screen.getByRole("img", {
      name: "上报，最近 3 天共 0 次，峰值 0 次（10-01）",
    });
    const bars = Array.from(chart.querySelectorAll("rect"));
    expect(bars).toHaveLength(3);
    for (const bar of bars) {
      expect(bar).toHaveAttribute("y", "30");
      expect(bar).toHaveAttribute("height", "2");
      expect(bar.getAttribute("class")).toContain("fill-border");
    }
    expect(container.querySelector("[style]")).toBeNull();

    const table = screen.getByRole("table", { name: "上报 每日数值（3 天）" });
    expect(within(table).getAllByText("0 次")).toHaveLength(3);
  });

  it("renders the empty label and no SVG when there are no buckets", () => {
    const { container } = render(<Sparkline data={[]} label="上报" emptyLabel="没有记录" />);

    expect(screen.getByText("没有记录")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(container.querySelector("svg")).toBeNull();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });
});
