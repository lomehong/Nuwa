import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ToastProvider, useToast } from "./Toast";

function Trigger() {
  const { toast } = useToast();
  return (
    <button
      type="button"
      onClick={() => toast({ title: "已保存", description: "设置已写入。", duration: 0 })}
    >
      显示通知
    </button>
  );
}

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
});
