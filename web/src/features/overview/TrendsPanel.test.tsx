import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { makeTrends } from "../../test/msw";
import { TrendsPanel } from "./TrendsPanel";

describe("TrendsPanel", () => {
  it("uses only dt and dd inside each definition-list group", () => {
    const { container } = render(
      <TrendsPanel
        trends={makeTrends({
          buckets: [{
            day: "2026-10-25",
            reports: 1,
            new_issues: 1,
            prs_opened: 1,
            merged: 0,
            rejected: 0,
            tokens: 100,
            ci_green: 1,
            ci_red: 0,
          }],
        })}
        isPending={false}
        error={null}
        onRetry={vi.fn()}
        range="14d"
        onRangeChange={vi.fn()}
      />,
    );

    const lists = container.querySelectorAll("dl");
    expect(lists.length).toBeGreaterThan(0);
    for (const list of lists) {
      for (const group of list.children) {
        for (const item of group.children) {
          expect(["DT", "DD"]).toContain(item.tagName);
        }
      }
    }
  });
});
