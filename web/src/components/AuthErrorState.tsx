import { CloudOff, RefreshCw } from "lucide-react";

import { Button, Card, CardContent } from "./ui";

/**
 * Full-page boot failure state (`AuthStatus = "error"`): the session check
 * could not reach `nuwa serve`, so sending the operator to the login form
 * would be misleading. Retry re-runs `GET /auth/me`.
 */
export function AuthErrorState({ onRetry }: { onRetry: () => void }) {
  return (
    <div className="flex min-h-screen items-center justify-center bg-bg px-4 py-10">
      <main className="w-full max-w-sm">
        <Card>
          <CardContent className="px-6 py-8">
            <div role="alert" className="flex flex-col items-center text-center">
              <span
                aria-hidden="true"
                className="flex size-12 items-center justify-center rounded-card border border-warning/40 bg-warning/10 text-warning"
              >
                <CloudOff className="size-6" />
              </span>
              <h1 className="mt-4 text-title font-semibold text-text">无法连接服务器</h1>
              <p className="mt-2 text-body-sm text-text-secondary">
                无法检查登录会话。请确认 <code className="font-mono">nuwa serve</code>{" "}
                正在运行，且网络可达；恢复后可以重试。
              </p>
            </div>
            <Button className="mt-5 w-full" onClick={onRetry}>
              <RefreshCw aria-hidden="true" className="size-4" />
              重试连接
            </Button>
            <p className="mt-5 border-t border-border pt-4 text-center text-caption text-text-secondary">
              如果服务已改用其它地址，请检查反向代理或 <code className="font-mono">--listen</code>{" "}
              配置。
            </p>
          </CardContent>
        </Card>
      </main>
    </div>
  );
}
