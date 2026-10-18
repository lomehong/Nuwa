import { describe, expect, it, vi } from "vitest";

import { createTestQueryClient } from "../test/render";
import { invalidateIssueViews, invalidateSettingsViews } from "./queryInvalidation";

function invalidatedKeys(client: ReturnType<typeof createTestQueryClient>) {
  const spy = vi.spyOn(client, "invalidateQueries");
  return { spy, keys: () => spy.mock.calls.map(([filters]) => filters?.queryKey) };
}

describe("invalidateIssueViews", () => {
  it("invalidates every view affected by an issue write", () => {
    const client = createTestQueryClient();
    const { keys } = invalidatedKeys(client);

    invalidateIssueViews(client, "fp-1");

    expect(keys()).toEqual(
      expect.arrayContaining([
        ["overview"],
        ["issues"],
        ["issue", "fp-1"],
        ["events"],
        ["trends"],
        ["runners"],
      ]),
    );
  });

  it("skips the single-issue key when no fingerprint is given", () => {
    const client = createTestQueryClient();
    const { keys } = invalidatedKeys(client);

    invalidateIssueViews(client);

    const invalidated = keys();
    expect(invalidated).toEqual(
      expect.arrayContaining([["overview"], ["issues"], ["events"], ["trends"], ["runners"]]),
    );
    expect(invalidated.some((key) => key?.[0] === "issue")).toBe(false);
  });
});

describe("invalidateSettingsViews", () => {
  it("invalidates settings, system and the audit log", () => {
    const client = createTestQueryClient();
    const { keys } = invalidatedKeys(client);

    invalidateSettingsViews(client);

    expect(keys()).toEqual(
      expect.arrayContaining([["settings"], ["system"], ["events"]]),
    );
  });
});
