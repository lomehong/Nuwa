import { AlertCircle, Gem } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Navigate, useLocation } from "react-router-dom";

import { AuthErrorState } from "../../components/AuthErrorState";
import { AuthLoading } from "../../components/AuthLoading";
import { Button, Card, CardContent, Input } from "../../components/ui";
import { ApiError } from "../../lib/api";
import { useAuth } from "../../lib/auth";

/** Only same-origin absolute paths from router state are accepted. */
function intendedPath(state: unknown): string {
  if (typeof state === "object" && state !== null) {
    const from = (state as { from?: unknown }).from;
    if (
      typeof from === "string" &&
      from.startsWith("/") &&
      !from.startsWith("//") &&
      from !== "/login"
    ) {
      return from;
    }
  }
  return "/";
}

function loginErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.status) {
      case 401:
        // Never distinguish "no such token" from "wrong token".
        return "令牌不正确。请检查 NUWA_UI_TOKEN 后重试。";
      case 403:
        return "请求被拒绝（CSRF 校验失败）。请刷新页面后重试。";
      case 429:
        return error.retryAfterSeconds
          ? `尝试过于频繁，请在 ${error.retryAfterSeconds} 秒后重试。`
          : "尝试过于频繁，请稍后重试。";
      default:
        return error.message || "登录失败，请稍后重试。";
    }
  }
  return "无法连接服务器。请确认 nuwa serve 正在运行，且网络可达。";
}

/**
 * Centered login card (docs/09 §10.2). The token is posted to
 * `/auth/login`; success sets HttpOnly cookies and returns to the route the
 * visitor originally asked for (`location.state.from`).
 */
export function LoginPage() {
  const { status, login, retry } = useAuth();
  const location = useLocation();
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  if (status === "loading") {
    return <AuthLoading />;
  }

  if (status === "error") {
    return <AuthErrorState onRetry={retry} />;
  }

  if (status === "authenticated") {
    return <Navigate to={intendedPath(location.state)} replace />;
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (pending) {
      return;
    }
    const value = token.trim();
    if (!value) {
      setError("请输入访问令牌。");
      return;
    }
    setError(null);
    setPending(true);
    try {
      await login(value);
      // The auth state flip renders the <Navigate> above.
    } catch (submitError) {
      setError(loginErrorMessage(submitError));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-bg px-4 py-10">
      <main className="w-full max-w-sm">
        <Card>
          <CardContent className="px-6 py-8">
            <div className="flex flex-col items-center text-center">
              <span
                aria-hidden="true"
                className="flex size-12 items-center justify-center rounded-card border border-accent/40 bg-accent/10 text-accent"
              >
                <Gem className="size-6" />
              </span>
              <h1 className="mt-4 text-title font-semibold text-text">女娲 · 补天</h1>
              <p className="mt-1 text-body-sm text-text-secondary">Nuwa 修复控制台</p>
            </div>

            <form onSubmit={handleSubmit} className="mt-6 space-y-4" noValidate>
              <Input
                label="访问令牌"
                type="password"
                autoComplete="current-password"
                autoFocus
                required
                value={token}
                onChange={(event) => setToken(event.target.value)}
                placeholder="输入服务端配置的 UI 令牌"
                hint="令牌只发送给本机 nuwa serve，会话保存在 HttpOnly Cookie 中。"
                aria-describedby={error ? "login-error" : undefined}
              />

              {error ? (
                <p
                  id="login-error"
                  role="alert"
                  className="flex items-start gap-2 rounded-card border border-danger/40 bg-danger/10 px-3 py-2 text-body-sm text-text"
                >
                  <AlertCircle aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-danger" />
                  <span>{error}</span>
                </p>
              ) : null}

              <Button type="submit" pending={pending} className="w-full">
                进入控制台
              </Button>
            </form>

            <p className="mt-5 border-t border-border pt-4 text-center text-caption text-text-secondary">
              本地模式（未配置 NUWA_UI_TOKEN）无需登录。
            </p>
          </CardContent>
        </Card>
      </main>
    </div>
  );
}
