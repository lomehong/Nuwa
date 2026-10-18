import { describe, expect, it } from "vitest";

import { isoDateTime, isValidTimestamp } from "./time";

describe("timestamp guards", () => {
  it("converts a valid epoch to an ISO string", () => {
    expect(isValidTimestamp(0)).toBe(true);
    expect(isoDateTime(0)).toBe("1970-01-01T00:00:00.000Z");
  });

  it("rejects non-finite and out-of-range timestamps", () => {
    const invalid = [
      Number.NaN,
      Number.POSITIVE_INFINITY,
      8_640_000_000_000_001,
      -8_640_000_000_000_001,
    ];
    for (const tsMs of invalid) {
      expect(isValidTimestamp(tsMs), String(tsMs)).toBe(false);
      expect(isoDateTime(tsMs), String(tsMs)).toBeUndefined();
    }
  });
});
