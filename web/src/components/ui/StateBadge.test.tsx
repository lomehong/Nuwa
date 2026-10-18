import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ISSUE_STATE_VALUES, issueStateLabel, StateBadge } from "./StateBadge";

describe("StateBadge", () => {
  it.each([...ISSUE_STATE_VALUES])("maps %s to a class, data attribute and label", (state) => {
    const { container } = render(<StateBadge state={state} />);
    const badge = container.querySelector(".state-badge");

    expect(badge).not.toBeNull();
    expect(badge).toHaveClass(`state-${state}`);
    expect(badge).toHaveAttribute("data-state", state);
    expect(screen.getByText(issueStateLabel(state))).toBeInTheDocument();
  });

  it("keeps a visible text label, not only colour", () => {
    render(<StateBadge state="merged" />);

    expect(screen.getByText("已合并")).toBeVisible();
  });
});
