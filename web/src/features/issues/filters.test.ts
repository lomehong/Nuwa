import { describe, expect, it } from "vitest";

import {
  buildIssueSearchParams,
  canonicalStates,
  hasActiveFilters,
  issueListPath,
  parseIssueFilters,
} from "./filters";
import { DEFAULT_ISSUE_FILTERS, type IssueListFilters } from "./types";

describe("issue list filters", () => {
  it("parses defaults from an empty query", () => {
    expect(parseIssueFilters(new URLSearchParams())).toEqual(DEFAULT_ISSUE_FILTERS);
  });

  it("canonicalizes states and ignores unknown filter values", () => {
    const filters = parseIssueFilters(
      new URLSearchParams(
        "state=fixing,bogus,new,fixing&severity=urgent&page=0&page_size=999" +
          "&sort=title&order=up&stuck=true&attention=0&q=%20panic%20" +
          "&repo=%20example.com%2Fapp%20&language=go",
      ),
    );

    expect(filters.states).toEqual(["new", "fixing"]);
    expect(filters.severity).toBe("");
    expect(filters.page).toBe(1);
    expect(filters.pageSize).toBe(50);
    expect(filters.sort).toBe("last_seen");
    expect(filters.order).toBe("desc");
    expect(filters.stuck).toBe(true);
    expect(filters.attention).toBe(false);
    expect(filters.q).toBe("panic");
    expect(filters.repo).toBe("example.com/app");
    expect(filters.language).toBe("go");
  });

  it("accepts page_size up to 200 and only 1-based pages", () => {
    const max = parseIssueFilters(new URLSearchParams("page=3&page_size=200"));
    expect(max.page).toBe(3);
    expect(max.pageSize).toBe(200);

    const tooLarge = parseIssueFilters(new URLSearchParams("page_size=201"));
    expect(tooLarge.pageSize).toBe(50);

    const negative = parseIssueFilters(new URLSearchParams("page=-2"));
    expect(negative.page).toBe(1);
  });

  it("omits defaults and round-trips non-default values", () => {
    const filters: IssueListFilters = {
      ...DEFAULT_ISSUE_FILTERS,
      states: ["fixing", "new"],
      severity: "high",
      repo: "example.com/app",
      language: "go",
      q: "panic: boom",
      stuck: true,
      attention: true,
      sort: "occurrences",
      order: "asc",
      page: 2,
      pageSize: 100,
    };

    const params = buildIssueSearchParams(filters);
    expect(params.get("state")).toBe("new,fixing");
    expect(params.get("page")).toBe("2");
    expect(params.get("page_size")).toBe("100");
    // Defaults (last_seen/desc/page 1/page_size 50) never reach the URL.
    expect(buildIssueSearchParams(DEFAULT_ISSUE_FILTERS).toString()).toBe("");

    expect(parseIssueFilters(params)).toEqual({ ...filters, states: ["new", "fixing"] });
  });

  it("builds the request path from the same non-default parameters", () => {
    expect(issueListPath(DEFAULT_ISSUE_FILTERS)).toBe("/issues");
    expect(
      issueListPath({ ...DEFAULT_ISSUE_FILTERS, states: ["escalated"], page: 3 }),
    ).toBe("/issues?state=escalated&page=3");
  });

  it("deduplicates and orders states by the lifecycle enum", () => {
    expect(canonicalStates(["escalated", "new", "escalated", "nope"])).toEqual([
      "new",
      "escalated",
    ]);
  });

  it("detects active filters but ignores sort and pagination", () => {
    expect(hasActiveFilters(DEFAULT_ISSUE_FILTERS)).toBe(false);
    expect(
      hasActiveFilters({ ...DEFAULT_ISSUE_FILTERS, sort: "state", page: 4, pageSize: 100 }),
    ).toBe(false);
    expect(hasActiveFilters({ ...DEFAULT_ISSUE_FILTERS, attention: true })).toBe(true);
    expect(hasActiveFilters({ ...DEFAULT_ISSUE_FILTERS, q: " panic " })).toBe(true);
  });

  it("caps the search term at the backend's 128-character limit", () => {
    const long = "x".repeat(129);
    expect(parseIssueFilters(new URLSearchParams(`q=${long}`)).q).toBe("x".repeat(128));

    const params = buildIssueSearchParams({ ...DEFAULT_ISSUE_FILTERS, q: long });
    expect(params.get("q")).toBe("x".repeat(128));

    // Shorter values keep their trimmed form.
    expect(
      buildIssueSearchParams({ ...DEFAULT_ISSUE_FILTERS, q: "  panic  " }).get("q"),
    ).toBe("panic");

    // The cap counts characters (code points), not UTF-16 units.
    const fox = "🦊".repeat(129);
    const capped = buildIssueSearchParams({ ...DEFAULT_ISSUE_FILTERS, q: fox }).get("q");
    expect(capped).not.toBeNull();
    expect(Array.from(capped ?? "")).toHaveLength(128);
  });
});
