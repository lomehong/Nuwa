import { Loader2 } from "lucide-react";

/** Full-page session-check state shared by the login route and auth gate. */
export function AuthLoading() {
  return (
    <div className="flex min-h-screen items-center justify-center bg-bg px-4">
      <div role="status" className="flex flex-col items-center gap-3 text-text-secondary">
        <Loader2 aria-hidden="true" className="size-6 animate-spin text-accent" />
        <p className="text-body-sm">正在检查会话…</p>
      </div>
    </div>
  );
}
