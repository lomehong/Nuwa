import { describe, expect, it } from "vitest";

import { eventKindLabel, formatDateTime, isoDateTime } from "./format";

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

  it("labels the lifecycle kinds the backend actually writes", () => {
    expect(eventKindLabel("merged")).toBe("已合并");
    expect(eventKindLabel("rejected")).toBe("已拒绝");
    expect(eventKindLabel("config_changed")).toBe("配置变更");
    expect(eventKindLabel("unknown_kind")).toBe("unknown_kind");
  });
});
