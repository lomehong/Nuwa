import { describe, expect, it } from "vitest";

import type { SettingDto } from "./types";
import { validateSettingValue } from "./validation";

function spec(overrides: Partial<SettingDto> = {}): SettingDto {
  return {
    key: "NUWA_TEST",
    label: "Test",
    group: "runner",
    type: "string",
    default: "",
    options: [],
    secret: false,
    scope: "work",
    requires_restart: true,
    read_only: false,
    help: "",
    min: null,
    max: null,
    ...overrides,
  };
}

describe("validateSettingValue", () => {
  const interval = spec({ key: "NUWA_INTERVAL", type: "int", min: 1, max: 86_400 });

  it("rejects non-integers and out-of-range numbers", () => {
    expect(validateSettingValue(interval, "abc")).toBe("请输入整数");
    expect(validateSettingValue(interval, "1.5")).toBe("请输入整数");
    expect(validateSettingValue(interval, "0")).toBe("必须在 1–86400 之间");
    expect(validateSettingValue(interval, "86401")).toBe("必须在 1–86400 之间");
    expect(validateSettingValue(interval, "30")).toBeNull();
    expect(validateSettingValue(interval, "1")).toBeNull();
    expect(validateSettingValue(interval, "86400")).toBeNull();
  });

  it("validates enums and URLs", () => {
    expect(validateSettingValue(spec({ type: "enum", options: ["a", "b"] }), "c")).toBe(
      "必须选择 a / b",
    );
    expect(validateSettingValue(spec({ type: "enum", options: ["a", "b"] }), "b")).toBeNull();
    expect(validateSettingValue(spec({ type: "url" }), "ftp://x")).toBe("必须是 http(s) URL");
    expect(validateSettingValue(spec({ type: "url" }), "https://example.test/v1")).toBeNull();
    expect(validateSettingValue(spec({ type: "url" }), "")).toBeNull();
  });

  it("leaves empty values and free text to the server", () => {
    expect(validateSettingValue(interval, "")).toBeNull();
    expect(validateSettingValue(spec({ type: "string" }), "任意 值")).toBeNull();
    expect(validateSettingValue(spec({ type: "csv" }), "a,b")).toBeNull();
  });

  it("bounds an int against whichever limit is present", () => {
    const onlyMin = spec({ type: "int", min: 10, max: null });
    expect(validateSettingValue(onlyMin, "9")).toBe("必须在 10–… 之间");
    expect(validateSettingValue(onlyMin, "10")).toBeNull();
    const onlyMax = spec({ type: "int", min: null, max: 10 });
    expect(validateSettingValue(onlyMax, "11")).toBe("必须在 …–10 之间");
  });
});
