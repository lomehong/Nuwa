import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SEVERITY_VALUES, SeverityStone, severityLabel } from "./SeverityStone";

describe("SeverityStone", () => {
  it.each([...SEVERITY_VALUES])("maps %s to a class, data attribute and label", (severity) => {
    const { container } = render(<SeverityStone severity={severity} />);
    const stone = container.querySelector(".severity-stone");

    expect(stone).not.toBeNull();
    expect(stone).toHaveClass(`sev-${severity}`);
    expect(stone).toHaveAttribute("data-severity", severity);
    expect(screen.getByText(severityLabel(severity))).toBeInTheDocument();
  });

  it("keeps the label available to screen readers when hidden", () => {
    render(<SeverityStone severity="critical" hideLabel />);

    expect(screen.getByText("严重")).toHaveClass("sr-only");
  });
});
