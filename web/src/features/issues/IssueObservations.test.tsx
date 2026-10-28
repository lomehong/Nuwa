import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { SignalObservationDto } from "../../lib/api-types/SignalObservationDto";
import { makeIssue, makeIssueDetail, makeSignalObservation } from "../../test/msw";
import { formatDateTime, isoDateTime } from "../overview/format";
import { IssueObservations } from "./IssueObservations";

describe("IssueObservations", () => {
  it("shows receipt and collector times, source confidence and metadata as text", () => {
    const hostile = '<img src=x onerror="alert(1)">';
    const sha = "0123456789abcdef".repeat(4);
    const untrusted = makeSignalObservation({
      id: 2,
      observed_at_ms: 1_700_000_000_000,
      time_status: "untrusted",
      environment: hostile,
      release_sha: sha,
      source_event_id: "event-<123>",
    });
    const trusted = makeSignalObservation({
      id: 1,
      trusted_source: "owner-import",
      observed_at_ms: 1_699_999_000_000,
      time_status: "trusted",
      disposition: "duplicate",
      duplicate_of: 7,
    });
    const { container } = render(
      <IssueObservations detail={makeIssueDetail({ observations: [untrusted, trusted] })} />,
    );

    const rows = within(screen.getByRole("region", { name: "回执记录" })).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]).getByText("来源未授信")).toBeInTheDocument();
    expect(within(rows[0]).getByText("未授信")).toHaveAttribute("title", "untrusted");
    expect(within(rows[0]).getByText(hostile)).toBeInTheDocument();
    expect(within(rows[0]).getByText(sha)).toBeInTheDocument();
    expect(within(rows[0]).getByText("event-<123>")).toBeInTheDocument();
    expect(within(rows[0]).getByText(formatDateTime(untrusted.received_at_ms)))
      .toHaveAttribute("datetime", isoDateTime(untrusted.received_at_ms));
    expect(within(rows[0]).getByText(formatDateTime(untrusted.observed_at_ms!)))
      .toHaveAttribute("datetime", isoDateTime(untrusted.observed_at_ms!));
    expect(within(rows[1]).getByText("来源已授信")).toBeInTheDocument();
    expect(within(rows[1]).getByText("已授信")).toHaveAttribute("title", "trusted");
    expect(within(rows[1]).getByText("owner-import")).toBeInTheDocument();
    expect(within(rows[1]).getByText("重复事件")).toBeInTheDocument();
    expect(within(rows[1]).getByText("#7")).toBeInTheDocument();
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
  });

  it("keeps deployment and recovery unknown even for a merged issue with green CI", () => {
    render(<IssueObservations detail={makeIssueDetail({
      issue: makeIssue({ state: "merged", ci_verdict: "success" }),
    })} />);

    expect(screen.getByText("部署状态：未知")).toBeInTheDocument();
    expect(screen.getByText("恢复状态：未知")).toBeInTheDocument();
    expect(screen.getByText(/PR 已合并不等于已部署，也不等于运行时已恢复/)).toBeInTheDocument();
  });

  it("handles an old response without receipt fields and nullable legacy metadata", () => {
    const { rerender } = render(<IssueObservations detail={{}} />);
    expect(screen.getByText(/暂无信号回执/)).toBeInTheDocument();
    expect(screen.getByText("部署状态：未知")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "回执记录" })).not.toBeInTheDocument();

    rerender(<IssueObservations detail={makeIssueDetail({ observations: [makeSignalObservation()] })} />);
    const row = screen.getByRole("listitem");
    expect(within(row).getByText("来源未授信")).toBeInTheDocument();
    expect(within(row).getAllByText("未提供")).toHaveLength(6);
    expect(within(row).queryByText("重复自回执")).not.toBeInTheDocument();
  });

  it("does not treat an attested source as proof of a valid collector time", () => {
    render(<IssueObservations detail={makeIssueDetail({ observations: [makeSignalObservation({
      trusted_source: "owner-import",
      observed_at_ms: 0,
      time_status: "invalid",
    })] })} />);

    expect(screen.getByText("来源已授信")).toBeInTheDocument();
    expect(screen.getByText("无效")).toHaveAttribute("title", "invalid");
    expect(screen.getByText("无效时间（0 ms）")).toBeInTheDocument();
  });

  it.each([
    ["delayed", "延迟上报", "future", "晚于接收时间"],
    ["muted", "已静默", "missing", "未提供"],
    ["regression", "回归上报", "trusted", "已授信"],
    ["cooldown", "冷却期上报", "untrusted", "未授信"],
    ["deduped", "重复上报（聚合）", "invalid", "无效"],
  ])("labels %s receipts without guessing the time status", (disposition, label, timeStatus, timeLabel) => {
    const { container } = render(<IssueObservations detail={makeIssueDetail({ observations: [makeSignalObservation({
      disposition,
      time_status: timeStatus,
    })] })} />);

    expect(screen.getByText(label)).toHaveAttribute("title", disposition);
    expect(container.querySelector(`[title="${timeStatus}"]`)).toHaveTextContent(timeLabel);
  });

  it("bounds the visible history to the latest 100 receipts in server order", () => {
    const observations: SignalObservationDto[] = Array.from({ length: 105 }, (_, index) =>
      makeSignalObservation({ id: 105 - index, received_at_ms: 1_700_000_000_000 - index }),
    );
    render(<IssueObservations detail={makeIssueDetail({ observations })} />);

    const history = screen.getByRole("region", { name: "回执记录" });
    const rows = within(history).getAllByRole("listitem");
    expect(rows).toHaveLength(100);
    expect(rows[0]).toHaveTextContent("回执 #105");
    expect(rows[99]).toHaveTextContent("回执 #6");
    expect(screen.queryByText("回执 #5")).not.toBeInTheDocument();
    expect(history).toHaveClass("max-h-96", "overflow-y-auto");
    expect(screen.getByText("最近 100 条")).toBeInTheDocument();
  });
});