import { AlertTriangle, RefreshCw } from "lucide-react";
import { Component, type ErrorInfo, type ReactNode } from "react";

export interface ErrorBoundaryProps {
  children: ReactNode;
  /** Test seam; defaults to a full page reload. */
  onReload?: () => void;
}

interface ErrorBoundaryState {
  error: Error | null;
}

/**
 * App-level render error boundary. A rendering bug in any page must not leave
 * a blank document: the fallback explains what happened and offers a reload,
 * while the error detail stays in the browser console (never rendered, so no
 * stack traces or local paths leak into the UI).
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("Nuwa console render error", error, info.componentStack);
  }

  private readonly handleReload = (): void => {
    if (this.props.onReload) {
      this.props.onReload();
      return;
    }
    window.location.reload();
  };

  render(): ReactNode {
    if (!this.state.error) {
      return this.props.children;
    }

    return (
      <div className="flex min-h-screen items-center justify-center bg-bg px-4 py-10">
        <main className="w-full max-w-md">
          <section
            className="rounded-card border border-danger/40 bg-surface p-6 text-text"
          >
            {/* The alert announces the failure only; the recovery actions are
                siblings so they are not part of the live region. */}
            <div role="alert" className="flex items-start gap-3">
              <span
                aria-hidden="true"
                className="flex size-10 shrink-0 items-center justify-center rounded-full bg-danger/10 text-danger"
              >
                <AlertTriangle className="size-5" />
              </span>
              <div className="min-w-0">
                <h1 className="text-heading font-semibold">页面渲染出错</h1>
                <p className="mt-1 text-body-sm text-text-secondary">
                  控制台遇到了未预期的错误，当前页面无法继续显示。刷新后重试；如果问题持续出现，请查看
                  serve 日志与浏览器控制台。
                </p>
              </div>
            </div>
            <div className="mt-5 flex flex-wrap gap-2">
              <button
                type="button"
                onClick={this.handleReload}
                className="ui-button inline-flex min-h-11 items-center justify-center gap-2 rounded-card border border-accent bg-accent px-4 text-body font-medium text-on-accent transition-colors duration-150 ease-out hover:bg-accent/90"
              >
                <RefreshCw aria-hidden="true" className="size-4" />
                重新加载页面
              </button>
              <a
                href="/"
                className="inline-flex min-h-11 items-center justify-center rounded-card border border-border-strong bg-raised px-4 text-body font-medium text-text transition-colors duration-150 ease-out hover:border-accent hover:text-accent"
              >
                返回总览
              </a>
            </div>
          </section>
        </main>
      </div>
    );
  }
}
