import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "./ConfirmDialog";

describe("ConfirmDialog", () => {
  it("renders nothing while closed and opens with an accessible name", () => {
    const { rerender } = render(
      <ConfirmDialog open={false} title="删除问题单？" onConfirm={() => {}} onCancel={() => {}} />,
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    rerender(
      <ConfirmDialog open title="删除问题单？" onConfirm={() => {}} onCancel={() => {}} />,
    );
    expect(screen.getByRole("dialog", { name: "删除问题单？" })).toBeInTheDocument();
  });

  it("calls onConfirm and onCancel from the buttons and focuses cancel first", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        open
        title="删除问题单？"
        description="删除后不可恢复。"
        confirmLabel="删除"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    expect(screen.getByRole("button", { name: "取消" })).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("closes on Escape", () => {
    const onCancel = vi.fn();
    render(<ConfirmDialog open title="删除问题单？" onConfirm={() => {}} onCancel={onCancel} />);

    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });

    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("wraps Tab and Shift+Tab inside the dialog", () => {
    render(<ConfirmDialog open title="删除问题单？" onConfirm={() => {}} onCancel={() => {}} />);

    const dialog = screen.getByRole("dialog");
    const close = screen.getByRole("button", { name: "关闭对话框" });
    const cancel = screen.getByRole("button", { name: "取消" });
    const confirm = screen.getByRole("button", { name: "确认" });

    expect(cancel).toHaveFocus();

    confirm.focus();
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(close).toHaveFocus();

    close.focus();
    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });
    expect(confirm).toHaveFocus();
  });

  it("inerts the page and locks body scrolling while open", () => {
    const { container, rerender } = render(
      <ConfirmDialog open title="删除问题单？" onConfirm={() => {}} onCancel={() => {}} />,
    );

    expect(document.body).toHaveClass("nuwa-scroll-lock");
    expect(container).toHaveAttribute("inert");

    rerender(
      <ConfirmDialog
        open={false}
        title="删除问题单？"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    expect(document.body).not.toHaveClass("nuwa-scroll-lock");
    expect(container).not.toHaveAttribute("inert");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("reference-counts the page lock across two concurrent modals", () => {
    const { container, rerender } = render(
      <>
        <ConfirmDialog open title="外层确认" onConfirm={() => {}} onCancel={() => {}} />
        <ConfirmDialog open title="内层确认" onConfirm={() => {}} onCancel={() => {}} />
      </>,
    );

    expect(document.body).toHaveClass("nuwa-scroll-lock");
    expect(container).toHaveAttribute("inert");
    // Neither panel may end up inside an inert backdrop.
    expect(screen.getByRole("dialog", { name: "外层确认" }).closest("[inert]")).toBeNull();
    expect(screen.getByRole("dialog", { name: "内层确认" }).closest("[inert]")).toBeNull();

    rerender(
      <>
        <ConfirmDialog open title="外层确认" onConfirm={() => {}} onCancel={() => {}} />
        <ConfirmDialog
          open={false}
          title="内层确认"
          onConfirm={() => {}}
          onCancel={() => {}}
        />
      </>,
    );

    // Closing the inner modal leaves the page locked and the outer modal
    // interactive; only the last unlock restores everything.
    expect(document.body).toHaveClass("nuwa-scroll-lock");
    expect(container).toHaveAttribute("inert");
    expect(screen.getByRole("dialog", { name: "外层确认" }).closest("[inert]")).toBeNull();

    rerender(
      <>
        <ConfirmDialog
          open={false}
          title="外层确认"
          onConfirm={() => {}}
          onCancel={() => {}}
        />
        <ConfirmDialog
          open={false}
          title="内层确认"
          onConfirm={() => {}}
          onCancel={() => {}}
        />
      </>,
    );

    expect(document.body).not.toHaveClass("nuwa-scroll-lock");
    expect(container).not.toHaveAttribute("inert");
  });
});
