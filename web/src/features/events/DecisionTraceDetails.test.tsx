import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { DecisionTraceDetails } from "./DecisionTraceDetails";

describe("决策 trace 详情", () => {
  it("expands to the raw structured payload", () => {
    const detail = JSON.stringify({
      stage: "repair",
      decision: "started",
      reason: "issue claimed",
      details: { skills: ["rust-cargo@1"], max_turns: 12 },
    });
    render(<DecisionTraceDetails detail={detail} />);

    expect(screen.getByText("决策详情")).toBeInTheDocument();
    fireEvent.click(screen.getByText("决策详情"));
    expect(screen.getByText(/"skills"/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "复制决策 trace JSON" })).toBeInTheDocument();
  });

  it("renders nothing for a malformed trace", () => {
    render(<DecisionTraceDetails detail="not json" />);
    expect(screen.queryByText("决策详情")).not.toBeInTheDocument();
  });
});
