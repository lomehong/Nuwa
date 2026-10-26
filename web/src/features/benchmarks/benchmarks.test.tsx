import { fireEvent, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  makeBenchmarkDetail,
  makeBenchmarkResult,
  makeBenchmarkRun,
  makeBenchmarks,
  makeScenario,
  mockBenchmarkDetail,
  mockBenchmarks,
  mockMe,
  mockOverview,
  server,
} from "../../test/msw";
import { renderApp } from "../../test/render";

const RUN = makeBenchmarkRun({
  id: "bench-1",
  scenarios: 2,
  repaired: 1,
  claimed: 2,
  false_positive: 1,
  delivery_ok: 1,
});
const OLDER = makeBenchmarkRun({
  id: "bench-0",
  started_at_ms: RUN.started_at_ms - 60_000,
  scenarios: 2,
  repaired: 2,
  claimed: 2,
  false_positive: 0,
  delivery_ok: 2,
});

const SCENARIOS = [
  makeScenario(),
  makeScenario({
    name: "node-noop-claim",
    title: "负对照：声称修好但只写了说明",
    expect_fixed: false,
    failing_check: "node --test",
  }),
];

describe("修复基准台账", () => {
  it("shows the latest score, run table and scenario catalog", async () => {
    server.use(
      mockMe(),
      mockOverview(),
      mockBenchmarks(makeBenchmarks({ data: [RUN, OLDER], scenarios: SCENARIOS })),
    );
    renderApp("/benchmarks");

    expect(
      await screen.findByRole(
        "heading",
        { level: 2, name: "修复基准" },
        { timeout: 5000 },
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("nuwa benchmark run --provider faux")).toBeInTheDocument();
    expect(await screen.findByText("误报率")).toBeInTheDocument();
    expect(screen.getAllByText("50.0%").length).toBeGreaterThan(0);
    expect(screen.getByText(/1 \/ 2 次声称/)).toBeInTheDocument();
    expect(screen.getAllByRole("link", { name: /查看/ }).length).toBe(2);
    expect(screen.getAllByText("node-empty-average").length).toBeGreaterThan(0);
    expect(screen.getByText("负对照")).toBeInTheDocument();
    expect(screen.getByText("node-noop-claim")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "failed" } });
    expect(screen.getByText("没有匹配的运行")).toBeInTheDocument();
  });
});

describe("基准运行详情", () => {
  it("separates false positives, expectation mismatches and gate blocks", async () => {
    const detail = makeBenchmarkDetail({
      run: RUN,
      results: [
        makeBenchmarkResult(),
        makeBenchmarkResult({
          scenario: "node-noop-claim",
          expected_fixed: false,
          repaired: false,
          claimed_fixed: true,
          delivery_ok: false,
          matches_expectation: true,
          root_cause: null,
        }),
        makeBenchmarkResult({
          scenario: "rust-wrong-operator",
          language: "rust",
          expected_fixed: true,
          repaired: false,
          claimed_fixed: false,
          delivery_ok: false,
          matches_expectation: false,
        }),
      ],
    });
    server.use(
      mockMe(),
      mockOverview(),
      mockBenchmarks(makeBenchmarks({ data: [RUN, OLDER], scenarios: SCENARIOS })),
      mockBenchmarkDetail(RUN.id, detail),
    );
    renderApp(`/benchmarks/${RUN.id}`);

    expect(
      await screen.findByRole(
        "heading",
        { level: 2, name: "基准运行" },
        { timeout: 5000 },
      ),
    ).toBeInTheDocument();
    expect(await screen.findByText("门禁正确拦截了这次误报")).toBeInTheDocument();
    expect(screen.getAllByText("误报").length).toBeGreaterThan(0);
    expect(screen.getByText("与预期不符")).toBeInTheDocument();
    expect(screen.getAllByText(/1 个场景/).length).toBeGreaterThan(0);
    expect(screen.getByText("node-empty-average")).toBeInTheDocument();
    expect(screen.getByText("node-noop-claim")).toBeInTheDocument();
    expect(screen.getByText("rust-wrong-operator")).toBeInTheDocument();
  });
});
