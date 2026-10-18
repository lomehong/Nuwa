import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { describe, expect, it } from "vitest";

import { PipelineFunnel } from "./PipelineFunnel";

function LocationProbe() {
  const location = useLocation();
  return <p data-testid="location">{`${location.pathname}${location.search}`}</p>;
}

const COUNTS = {
  new: 2,
  triggered: 1,
  fixing: 1,
  pr_open: 7,
  merged: 1,
  rejected: 0,
  escalated: 3,
};

describe("PipelineFunnel", () => {
  it("links every stage to the filtered issue list and navigates on click", () => {
    render(
      <MemoryRouter initialEntries={["/"]}>
        <PipelineFunnel counts={COUNTS} />
        <LocationProbe />
      </MemoryRouter>,
    );

    expect(screen.getByRole("link", { name: "新建：2 条，查看问题单" })).toHaveAttribute(
      "href",
      "/issues?state=new",
    );
    expect(
      screen.getByRole("link", { name: "PR 待合并：7 条，查看问题单" }),
    ).toHaveAttribute("href", "/issues?state=pr_open");

    fireEvent.click(screen.getByRole("link", { name: "已升级：3 条，查看问题单" }));

    expect(screen.getByTestId("location")).toHaveTextContent("/issues?state=escalated");
  });

  it("renders zero counts without hiding the stage", () => {
    render(
      <MemoryRouter>
        <PipelineFunnel counts={{}} />
      </MemoryRouter>,
    );

    expect(screen.getByRole("link", { name: "已拒绝：0 条，查看问题单" })).toBeInTheDocument();
    expect(screen.getByText(/问题单总数/)).toHaveTextContent("问题单总数 0");
  });
});
