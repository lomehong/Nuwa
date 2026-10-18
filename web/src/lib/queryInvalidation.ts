/**
 * Shared cache invalidation for console writes.
 *
 * Server-side changes ripple across panels: a reopen/requeue moves an issue
 * between states (overview counts, attention queue, list rows, detail), writes
 * an audit event and may free a runner. Invalidating the broad prefixes keeps
 * every view honest without teaching each mutation about each query key.
 */

import type { QueryClient } from "@tanstack/react-query";

export function invalidateIssueViews(queryClient: QueryClient, fingerprint?: string): void {
  void queryClient.invalidateQueries({ queryKey: ["overview"] });
  void queryClient.invalidateQueries({ queryKey: ["issues"] });
  if (fingerprint) {
    void queryClient.invalidateQueries({ queryKey: ["issue", fingerprint] });
  }
  void queryClient.invalidateQueries({ queryKey: ["events"] });
  void queryClient.invalidateQueries({ queryKey: ["trends"] });
  void queryClient.invalidateQueries({ queryKey: ["runners"] });
}

export function invalidateSettingsViews(queryClient: QueryClient): void {
  void queryClient.invalidateQueries({ queryKey: ["settings"] });
  void queryClient.invalidateQueries({ queryKey: ["system"] });
  void queryClient.invalidateQueries({ queryKey: ["events"] });
}
