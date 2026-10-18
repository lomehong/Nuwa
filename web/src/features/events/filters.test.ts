import { describe, expect, it } from "vitest";

import {
  DEFAULT_EVENT_FILTERS,
  eventPagePath,
  eventQueryString,
  parseEventFilters,
} from "./filters";

const NOW = 1_700_000_000_000;

describe("event filters", () => {
  it("round-trips kinds, repo, range, aggregate and page", () => {
    const filters = parseEventFilters(
      new URLSearchParams(
        "kind=reported,state_changed&repo=example.com%2Fapp&fingerprint=abc&range=7d&aggregate=1&page=3",
      ),
    );
    expect(filters).toEqual({
      kinds: ["reported", "state_changed"],
      repo: "example.com/app",
      fingerprint: "abc",
      range: "7d",
      aggregate: true,
      page: 3,
    });
    expect(eventPagePath(filters)).toBe(
      "/events?kind=reported%2Cstate_changed&repo=example.com%2Fapp&fingerprint=abc&range=7d&aggregate=1&page=3",
    );
  });

  it("drops unknown kinds and falls back to 24h/page 1", () => {
    const filters = parseEventFilters(new URLSearchParams("kind=bogus&range=nope&page=0"));
    expect(filters.kinds).toEqual([]);
    expect(filters.range).toBe("24h");
    expect(filters.page).toBe(1);
  });

  it("keeps known kinds beside an unknown one", () => {
    const filters = parseEventFilters(
      new URLSearchParams("kind=bogus,reported,runner_error"),
    );
    expect(filters.kinds).toEqual(["reported"]);
  });

  it("deduplicates repeated kinds in the canonical path", () => {
    const filters = parseEventFilters(new URLSearchParams("kind=reported,reported"));
    expect(filters.kinds).toEqual(["reported"]);
    expect(eventPagePath(filters)).toBe("/events?kind=reported");
  });

  it("falls back for a non-numeric page and trims text filters", () => {
    const filters = parseEventFilters(
      new URLSearchParams("repo=%20example.com%2Fapp%20&fingerprint=%20abc%20&page=abc"),
    );
    expect(filters.repo).toBe("example.com/app");
    expect(filters.fingerprint).toBe("abc");
    expect(filters.page).toBe(1);
  });

  it("keeps defaults out of the page URL", () => {
    expect(parseEventFilters(new URLSearchParams())).toEqual(DEFAULT_EVENT_FILTERS);
    expect(eventPagePath(DEFAULT_EVENT_FILTERS)).toBe("/events");
  });

  it("computes the API window from the range inside the request", () => {
    const query = new URLSearchParams(
      eventQueryString(
        {
          ...DEFAULT_EVENT_FILTERS,
          kinds: ["reported"],
          range: "7d",
          aggregate: true,
        },
        NOW,
      ),
    );
    expect(query.get("kind")).toBe("reported");
    expect(query.get("from_ms")).toBe(String(NOW - 7 * 86_400_000));
    expect(query.get("aggregate")).toBe("1");
    expect(query.get("page")).toBe("1");
    expect(query.get("page_size")).toBe("50");
    expect(query.get("repo")).toBeNull();
    expect(query.get("fingerprint")).toBeNull();
  });

  it("omits from_ms for the all range", () => {
    const query = new URLSearchParams(
      eventQueryString({ ...DEFAULT_EVENT_FILTERS, range: "all" }, NOW),
    );
    expect(query.get("from_ms")).toBeNull();
    expect(query.get("page_size")).toBe("50");
  });
});
