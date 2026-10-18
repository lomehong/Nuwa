import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ErrorBoundary } from "./ErrorBoundary";

function Boom(): never {
  throw new Error("boom");
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("ErrorBoundary", () => {
  it("renders children while healthy", () => {
    render(
      <ErrorBoundary>
        <p>正常内容</p>
      </ErrorBoundary>,
    );

    expect(screen.getByText("正常内容")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("renders a reloadable fallback when a child throws", () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const onReload = vi.fn();

    render(
      <ErrorBoundary onReload={onReload}>
        <Boom />
      </ErrorBoundary>,
    );

    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("页面渲染出错");
    expect(alert).not.toHaveTextContent("boom");
    // Recovery actions stay outside the live region.
    expect(within(alert).queryByRole("button")).not.toBeInTheDocument();
    expect(within(alert).queryByRole("link")).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "返回总览" })).toHaveAttribute("href", "/");

    fireEvent.click(screen.getByRole("button", { name: "重新加载页面" }));
    expect(onReload).toHaveBeenCalledTimes(1);
    expect(consoleError).toHaveBeenCalled();
  });
});
