import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ApiError } from "../../lib/api";
import { makeTranscript } from "../../test/msw";
import { TranscriptViewer, type TranscriptViewerProps } from "./TranscriptViewer";

const NOW = 1_700_000_000_000;

function renderViewer(overrides: Partial<TranscriptViewerProps> = {}) {
  const props: TranscriptViewerProps = {
    response: makeTranscript(),
    isPending: false,
    isFetching: false,
    error: null,
    runId: null,
    page: 1,
    onPageChange: vi.fn(),
    onRetry: vi.fn(),
    onClearRun: vi.fn(),
    ...overrides,
  };
  return { ...render(<TranscriptViewer {...props} />), props };
}

function summaryFor(text: string | RegExp): HTMLElement {
  const node = screen.getByText(text);
  const summary = node.closest("summary");
  if (!summary) {
    throw new Error(`no summary for ${String(text)}`);
  }
  return summary;
}

describe("TranscriptViewer", () => {
  it("groups roles, renders object arguments and collapses thinking/tool results", () => {
    renderViewer({
      response: makeTranscript({
        data: [
          {
            seq: 1,
            role: "user",
            ts_ms: NOW,
            blocks: [{ type: "text", text: "请修复 Average 的空切片崩溃。" }],
          },
          {
            seq: 2,
            role: "assistant",
            ts_ms: NOW + 1_000,
            blocks: [
              { type: "thinking", text: "先分析堆栈定位 calc.go。" },
              { type: "text", text: "我会读取 calc.go。" },
              {
                type: "tool_call",
                name: "read",
                arguments: { path: "calc.go", limit: 20 },
              },
            ],
          },
          {
            seq: 3,
            role: "tool",
            ts_ms: NOW + 2_000,
            blocks: [
              {
                type: "tool_result",
                name: "read",
                text: "package app\n\nfunc Average() {}",
                is_error: false,
              },
            ],
          },
        ],
        meta: {
          page: 1,
          page_size: 20,
          total: 3,
          truncated: true,
          empty: false,
        },
      }),
    });

    expect(screen.getByRole("heading", { name: "用户消息" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "助手消息" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "工具消息" })).toBeInTheDocument();
    expect(screen.getByText("请修复 Average 的空切片崩溃。")).toBeInTheDocument();
    expect(screen.getByText("我会读取 calc.go。")).toBeInTheDocument();
    expect(screen.getByText(/"path": "calc\.go"/)).toBeInTheDocument();
    expect(screen.getByText(/会话文件超过 2MB/)).toBeInTheDocument();

    const thinking = summaryFor("思考过程");
    expect(thinking.closest("details")).not.toHaveAttribute("open");
    fireEvent.click(thinking);
    expect(thinking.closest("details")).toHaveAttribute("open");

    const toolResult = summaryFor(/工具结果 · read/);
    expect(toolResult.closest("details")).not.toHaveAttribute("open");
    fireEvent.click(toolResult);
    expect(toolResult.closest("details")).toHaveAttribute("open");
    expect(screen.getByText(/package app/)).toBeInTheDocument();
  });

  it("renders a truncated string argument preview and collapses long text", () => {
    const longText = "长".repeat(1_000);
    renderViewer({
      response: makeTranscript({
        data: [
          {
            seq: 4,
            role: "assistant",
            ts_ms: NOW,
            blocks: [
              {
                type: "tool_call",
                name: "write",
                arguments: "<truncated: 20000 bytes>",
                truncated: true,
              },
              { type: "text", text: longText },
            ],
          },
        ],
        meta: {
          page: 1,
          page_size: 20,
          total: 1,
          truncated: false,
          empty: false,
        },
      }),
    });

    expect(screen.getByText("<truncated: 20000 bytes>")).toBeInTheDocument();
    expect(screen.getByText("参数已截断")).toBeInTheDocument();
    expect(screen.getByText(/参数体积过大/)).toBeInTheDocument();

    const expand = summaryFor(/展开全文（1000 字符）/);
    expect(expand.closest("details")).not.toHaveAttribute("open");
    fireEvent.click(expand);
    expect(expand.closest("details")).toHaveAttribute("open");
    expect(screen.getAllByText(new RegExp(longText.slice(0, 10))).length).toBeGreaterThan(0);
  });

  it("paginates messages and reports the current page", () => {
    const { props } = renderViewer({
      response: makeTranscript({
        data: [
          {
            seq: 1,
            role: "user",
            ts_ms: NOW,
            blocks: [{ type: "text", text: "第一页消息" }],
          },
        ],
        meta: {
          page: 1,
          page_size: 20,
          total: 45,
          truncated: false,
          empty: false,
        },
      }),
    });

    const pagination = screen.getByRole("navigation", { name: "会话分页" });
    expect(pagination).toHaveTextContent("第 1 / 3 页");
    expect(pagination).toHaveTextContent("共 45 条消息");
    expect(screen.getByRole("button", { name: "上一页" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    expect(props.onPageChange).toHaveBeenCalledWith(2);
  });

  it("offers a way back when the requested page is out of range", () => {
    const { props } = renderViewer({
      page: 4,
      response: makeTranscript({
        data: [],
        meta: {
          page: 4,
          page_size: 20,
          total: 45,
          truncated: false,
          empty: false,
        },
      }),
    });

    expect(screen.getByText("该页没有消息")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "回到第一页" }));
    expect(props.onPageChange).toHaveBeenCalledWith(1);
  });

  it("renders the empty state when no session exists", () => {
    renderViewer({ response: makeTranscript() });
    expect(screen.getByText("暂无会话转录")).toBeInTheDocument();
    expect(screen.getByText(/还没有可读取的会话转录/)).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "会话分页" })).not.toBeInTheDocument();
  });

  it("announces loading", () => {
    renderViewer({ response: undefined, isPending: true });
    expect(screen.getByText("正在加载会话转录")).toBeInTheDocument();
  });

  it("offers to clear a run that has no transcript", () => {
    const { props } = renderViewer({
      response: undefined,
      error: new ApiError(404, "not_found", "run not found"),
      runId: "run-missing",
    });

    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("所选运行没有会话转录");
    expect(alert).toHaveTextContent("not_found");
    // Recovery actions stay outside the live region.
    expect(within(alert).queryByRole("button")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "查看最近会话" }));
    expect(props.onClearRun).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });
});
