import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ToastProvider, useToast } from "./Toast";

function Trigger({ duration = 5000 }: { duration?: number }) {
  const { toast } = useToast();
  return (
    <button
      type="button"
      onClick={() => toast({ title: "已保存", description: "设置已写入。", duration })}
    >
      显示通知
    </button>
  );
}

function renderToast(duration = 5000) {
  render(
    <ToastProvider>
      <Trigger duration={duration} />
    </ToastProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: "显示通知" }));
}

function toastRoot(): HTMLElement {
  const root = screen.getByText("已保存").closest<HTMLElement>("[data-toast]");
  if (!root) {
    throw new Error("toast root not found");
  }
  return root;
}

afterEach(() => {
  vi.useRealTimers();
});

describe("Toast", () => {
  it("exposes a polite live region and renders the toast inside it", () => {
    render(
      <ToastProvider>
        <Trigger />
      </ToastProvider>,
    );

    const liveRegion = screen.getByRole("status", { name: "通知" });
    expect(liveRegion).toHaveAttribute("aria-live", "polite");

    fireEvent.click(screen.getByRole("button", { name: "显示通知" }));

    expect(screen.getByText("已保存")).toBeInTheDocument();
    expect(liveRegion).toContainElement(screen.getByText("已保存"));
  });

  it("dismisses a toast from its close button", () => {
    render(
      <ToastProvider>
        <Trigger />
      </ToastProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "显示通知" }));
    fireEvent.click(screen.getByRole("button", { name: "关闭通知：已保存" }));

    expect(screen.queryByText("已保存")).not.toBeInTheDocument();
  });

  it("pauses auto-dismiss while hovered and resumes with the remaining time", () => {
    vi.useFakeTimers();
    renderToast();

    act(() => {
      vi.advanceTimersByTime(3000);
    });

    fireEvent.pointerEnter(toastRoot());
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(screen.getByText("已保存")).toBeInTheDocument();

    fireEvent.pointerLeave(toastRoot());
    act(() => {
      vi.advanceTimersByTime(1999);
    });
    expect(screen.getByText("已保存")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText("已保存")).not.toBeInTheDocument();
  });

  it("pauses auto-dismiss while focus is inside the toast and the close button still works", () => {
    vi.useFakeTimers();
    renderToast();

    act(() => {
      vi.advanceTimersByTime(4000);
    });

    const close = screen.getByRole("button", { name: "关闭通知：已保存" });
    fireEvent.focusIn(close);
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(screen.getByText("已保存")).toBeInTheDocument();

    fireEvent.focusOut(close);
    act(() => {
      vi.advanceTimersByTime(999);
    });
    expect(screen.getByText("已保存")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText("已保存")).not.toBeInTheDocument();
  });

  it("keeps the close button reachable while a focused toast is paused", () => {
    vi.useFakeTimers();
    renderToast();

    const close = screen.getByRole("button", { name: "关闭通知：已保存" });
    fireEvent.focusIn(close);
    act(() => {
      vi.advanceTimersByTime(10_000);
    });

    fireEvent.click(close);
    expect(screen.queryByText("已保存")).not.toBeInTheDocument();
  });

  it("pauses auto-dismiss while the tab is hidden", () => {
    vi.useFakeTimers();
    renderToast();

    act(() => {
      vi.advanceTimersByTime(2000);
    });

    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(screen.getByText("已保存")).toBeInTheDocument();

    hidden.mockReturnValue(false);
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    act(() => {
      vi.advanceTimersByTime(2999);
    });
    expect(screen.getByText("已保存")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText("已保存")).not.toBeInTheDocument();
  });

  it("keeps sticky toasts (duration 0) until dismissed", () => {
    vi.useFakeTimers();
    renderToast(0);

    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByText("已保存")).toBeInTheDocument();
  });
});
