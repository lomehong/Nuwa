import { describe, expect, it } from "vitest";

import { formatDateTime, isoDateTime } from "./format";

describe("overview timestamp formatting", () => {
  it("returns an em dash for out-of-range timestamps", () => {
    for (const tsMs of [Number.NaN, 8_640_000_000_000_001, -8_640_000_000_000_001]) {
      expect(formatDateTime(tsMs), String(tsMs)).toBe("—");
    }
  });

  it("re-exports the shared ISO helper", () => {
    expect(isoDateTime(0)).toBe("1970-01-01T00:00:00.000Z");
    expect(isoDateTime(8_640_000_000_000_001)).toBeUndefined();
  });
});
