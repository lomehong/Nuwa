import { WifiOff } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";

import { Button } from "./ui";
import { useServerReachable } from "../lib/connectivity";

/**
 * Global offline banner (docs/09 §10.1 / §10.9). `apiFetch` reports transport
 * failures to `lib/connectivity`; any HTTP response clears the banner, so a
 * single successful poll removes it without a reload.
 */
export function OfflineBanner() {
  const reachable = useServerReachable();
  const queryClient = useQueryClient();

  if (reachable) {
    return null;
  }

  return (
    <div className="border-b border-warning/40 bg-warning/10 text-text">
      <div className="mx-auto flex w-full max-w-page flex-wrap items-center gap-x-3 gap-y-2 px-4 py-2 text-body-sm sm:px-6 lg:px-8">
        {/* The alert announces the outage only; the retry button stays an
            ordinary control outside the live region. */}
        <div role="alert" className="flex min-w-0 flex-1 items-start gap-2">
          <WifiOff aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-warning" />
          <p className="min-w-0">
            <span className="font-medium">无法连接服务器。</span>
            <span className="text-text-secondary">数据可能已过期；恢复后会自动刷新。</span>
          </p>
        </div>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => void queryClient.invalidateQueries()}
        >
          立即重试
        </Button>
      </div>
    </div>
  );
}
