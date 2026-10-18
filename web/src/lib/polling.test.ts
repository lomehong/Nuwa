import { describe, expect, it } from "vitest";

import { backoffDelay, backoffInterval } from "./polling";

describe("backoffDelay", () => {
  it("backs off exponentially and caps at 60s", () => {
    expect(backoffDelay(10_000, 0)).toBe(10_000);
    expect(backoffDelay(10_000, 1)).toBe(20_000);
    expect(backoffDelay(10_000, 2)).toBe(40_000);
    expect(backoffDelay(10_000, 3)).toBe(60_000);
    expect(backoffDelay(10_000, 50)).toBe(60_000);
  });

  it("never returns less than the base interval for negative counts", () => {
    expect(backoffDelay(15_000, -1)).toBe(15_000);
  });

  it("honours a custom cap", () => {
    expect(backoffDelay(5_000, 4, 12_000)).toBe(12_000);
    expect(backoffDelay(5_000, 1, 12_000)).toBe(10_000);
  });
});

describe("backoffInterval", () => {
  it("maps failure counts 0/1/2/3 to 10s/20s/40s/60s", () => {
    const interval = backoffInterval(10_000);

    expect(interval({ state: { fetchFailureCount: 0 } })).toBe(10_000);
    expect(interval({ state: { fetchFailureCount: 1 } })).toBe(20_000);
    expect(interval({ state: { fetchFailureCount: 2 } })).toBe(40_000);
    expect(interval({ state: { fetchFailureCount: 3 } })).toBe(60_000);
  });

  it("caps at 60s after many consecutive failures", () => {
    const interval = backoffInterval(10_000);

    expect(interval({ state: { fetchFailureCount: 50 } })).toBe(60_000);
  });
});
